//! Native playback clock/audio and a bounded host reader; GPUI retains only the current frame.

use anyhow::{Context as _, Result, ensure};
use avplayer::{
    AssetResourceLoaderEvent, AssetResourceLoaderObserver, AssetResourceLoadingRequest, Player,
    PlayerItem, PlayerItemVideoOutput, PlayerItemVideoOutputSettings, PlayerStatus, Time, UrlAsset,
};
use bootty_host::media::{MediaCancellation, MediaReader};
use core_foundation::base::TCFType as _;
use core_video::pixel_buffer::{CVPixelBuffer, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    slider::{Slider, SliderEvent, SliderState},
};
use gpui_kit::{
    Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window, div,
    prelude::*, surface,
};
use num_traits::ToPrimitive as _;
use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

pub struct Playback {
    player: Player,
    output: PlayerItemVideoOutput,
    _asset: UrlAsset,
    _loader: AssetResourceLoaderObserver,
    retired: Arc<AtomicBool>,
    cancellation: MediaCancellation,
}

impl Playback {
    /// Construct on a worker: native metadata loading may request file ranges.
    pub(crate) fn open(mut reader: MediaReader, content_type: &'static str) -> Result<Self> {
        let len = reader.len();
        let cancellation = reader.cancellation();
        let retired = Arc::new(AtomicBool::new(false));
        let worker_retired = retired.clone();
        // AVFoundation owns pending requests; keep our work queue bounded.
        let (sender, receiver) = mpsc::sync_channel::<AssetResourceLoadingRequest>(8);
        std::thread::Builder::new()
            .name("video-source".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    if worker_retired.load(Ordering::Acquire) {
                        break;
                    }
                    if let Err(error) =
                        supply_request(&mut reader, len, content_type, &request, &worker_retired)
                    {
                        let _ = request.finish_loading_with_error(error.to_string());
                    }
                }
            })?;
        // This private scheme never reaches a network URL loader.
        let asset = UrlAsset::from_remote_url("bootty-media://preview/video")?;
        let loader = asset.resource_loader().observe_loading_requests(
            Some("bootty.video.requests"),
            move |event| match event {
                AssetResourceLoaderEvent::LoadingRequested(request) => {
                    sender.try_send(request).is_ok()
                }
                AssetResourceLoaderEvent::LoadingCancelled(_) => true,
                AssetResourceLoaderEvent::RenewalRequested(_) => false,
            },
        )?;
        let item = PlayerItem::from_asset(asset.as_asset())?;
        item.set_preferred_forward_buffer_duration(5.);
        // Apply track rotation and presentation dimensions in the native compositor.
        item.set_video_composition_from_asset(asset.as_asset())?;
        let settings =
            PlayerItemVideoOutputSettings::new(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        let output = PlayerItemVideoOutput::new(Some(&settings))?;
        item.add_video_output(&output)?;
        let player = Player::from_item(&item)?;
        Ok(Self {
            player,
            output,
            _asset: asset,
            _loader: loader,
            retired,
            cancellation,
        })
    }

    #[allow(
        unsafe_code,
        reason = "Retain the same Core Video object in GPUI's wrapper"
    )]
    fn frame(&self) -> Result<Option<CVPixelBuffer>> {
        let time = self.player.current_time()?;
        if !self.output.has_new_pixel_buffer_for_item_time(time) {
            return Ok(None);
        }
        let Some(buffer) = self.output.copy_pixel_buffer_for_item_time(time) else {
            return Ok(None);
        };
        ensure!(
            buffer.pixel_format() == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            "unsupported video pixel format"
        );
        // SAFETY: AVPlayer returned an owned CVPixelBuffer. wrap_under_get_rule retains
        // that exact object before the source wrapper drops; neither wrapper mutates it.
        Ok(Some(unsafe {
            CVPixelBuffer::wrap_under_get_rule(buffer.as_ptr().cast())
        }))
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.player.pause();
        self.retired.store(true, Ordering::Release);
        self.cancellation.cancel();
    }
}

fn supply_request(
    reader: &mut MediaReader,
    len: u64,
    content_type: &str,
    request: &AssetResourceLoadingRequest,
    retired: &AtomicBool,
) -> Result<()> {
    if let Some(info) = request.content_information_request() {
        info.set_content_type(Some(content_type))?;
        info.set_content_length(i64::try_from(len)?);
        info.set_byte_range_access_supported(true);
    }
    if let Some(data) = request.data_request() {
        let offset = u64::try_from(data.current_offset()?)?;
        let end = if data
            .requests_all_data_to_end_of_resource()?
            .unwrap_or(false)
        {
            len
        } else {
            u64::try_from(data.requested_offset()?)?
                .checked_add(u64::try_from(data.requested_length()?)?)
                .context("invalid video range")?
                .min(len)
        };
        ensure!(offset <= end, "invalid video range");
        reader.seek(SeekFrom::Start(offset))?;
        let mut remaining = end.saturating_sub(offset);
        let mut bytes = vec![0u8; 64 * 1024];
        while remaining > 0 {
            if retired.load(Ordering::Acquire) || request.is_cancelled()? {
                return Ok(());
            }
            let count = usize::try_from(remaining.min(u64::try_from(bytes.len())?))?;
            let chunk = bytes.get_mut(..count).context("invalid video read size")?;
            reader.read_exact(chunk)?;
            data.respond_with_data(chunk);
            remaining = remaining.saturating_sub(u64::try_from(count)?);
        }
    }
    if !request.is_cancelled()? {
        request.finish_loading();
    }
    Ok(())
}

pub struct VideoPreview {
    playback: Playback,
    frame: Option<CVPixelBuffer>,
    seek: Entity<SliderState>,
    _seek_subscription: Subscription,
    playing: bool,
    scrubbing: bool,
    duration: f64,
    position: f64,
    error: Option<String>,
    awaiting_frame: bool,
}

impl VideoPreview {
    pub(crate) fn new(playback: Playback, cx: &mut Context<Self>) -> Self {
        let seek = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.001));
        let subscription = cx.subscribe(&seek, |this, _, event, cx| {
            match event {
                SliderEvent::Change(_) => this.scrubbing = true,
                SliderEvent::Release(value) => {
                    this.scrubbing = false;
                    let target = f64::from(value.start()) * this.duration;
                    this.seek_to(target);
                }
            }
            cx.notify();
        });
        Self {
            playback,
            frame: None,
            seek,
            _seek_subscription: subscription,
            playing: false,
            scrubbing: false,
            duration: 0.,
            position: 0.,
            error: None,
            awaiting_frame: true,
        }
    }

    pub(crate) fn pause(&mut self, cx: &mut Context<Self>) {
        self.playback.player.pause();
        self.playing = false;
        cx.notify();
    }

    fn seek_to(&mut self, seconds: f64) {
        self.awaiting_frame = true;
        let seconds = seconds.clamp(0., self.duration);
        let Some(value) = (seconds * 600.).to_i64() else {
            return;
        };
        if let Err(error) = self.playback.player.seek_to(Time::new(value, 600)) {
            self.error = Some(error.to_string());
        }
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        ensure!(
            self.playback.player.status()? != PlayerStatus::Failed,
            "video playback failed"
        );
        if let Some(frame) = self.playback.frame()? {
            self.frame = Some(frame);
            self.awaiting_frame = false;
        }
        self.duration = seconds(self.playback.player.duration()?).unwrap_or(0.);
        self.position = seconds(self.playback.player.current_time()?).unwrap_or(0.);
        if self.playing && self.duration > 0. && self.position >= self.duration {
            self.pause(cx);
        }
        if !self.scrubbing && self.duration > 0. {
            let value = (self.position / self.duration).to_f32().unwrap_or(0.);
            if (self.seek.read(cx).value().start() - value).abs() > 0.001 {
                self.seek
                    .update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
        if self.playing || self.awaiting_frame {
            window.request_animation_frame();
        }
        Ok(())
    }
}

fn seconds(time: Time) -> Option<f64> {
    let (value, scale) = time.as_numeric()?;
    (scale > 0)
        .then(|| value.to_f64().map(|value| value / f64::from(scale)))
        .flatten()
}

fn timestamp(seconds: f64) -> String {
    let seconds = seconds.max(0.).to_u64().unwrap_or(0);
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl Render for VideoPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.error.is_none()
            && let Err(error) = self.poll(window, cx)
        {
            self.playback.player.pause();
            self.playing = false;
            self.error = Some(error.to_string());
        }
        let content = if self.error.is_some() {
            div().child("Could not play this video.").into_any_element()
        } else if let Some(frame) = &self.frame {
            surface(frame.clone())
                .size_full()
                .object_fit(gpui_kit::ObjectFit::Contain)
                .into_any_element()
        } else {
            div().child("Loading video…").into_any_element()
        };
        let playback_label = if self.playing { "Pause" } else { "Play" };
        let muted = self.playback.player.is_muted().unwrap_or(false);
        let volume_label = if muted { "Unmute" } else { "Mute" };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().overflow_hidden().child(content))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .child(
                        Button::new("play-pause")
                            .small()
                            .ghost()
                            .icon(if self.playing {
                                IconName::Pause
                            } else {
                                IconName::Play
                            })
                            .tooltip(playback_label)
                            .accessibility_label(playback_label)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.playing {
                                    this.pause(cx);
                                } else {
                                    if this.duration > 0. && this.position >= this.duration {
                                        this.seek_to(0.);
                                    }
                                    this.playback.player.play();
                                    this.playing = true;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(div().text_xs().child(timestamp(self.position)))
                    .child(
                        Slider::new(&self.seek)
                            .w_full()
                            .disabled(self.duration <= 0.),
                    )
                    .child(div().text_xs().child(timestamp(self.duration)))
                    .child(
                        Button::new("mute-video")
                            .small()
                            .ghost()
                            .icon(if muted {
                                IconName::VolumeX
                            } else {
                                IconName::Volume2
                            })
                            .tooltip(volume_label)
                            .accessibility_label(volume_label)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.playback
                                    .player
                                    .set_muted(!this.playback.player.is_muted().unwrap_or(false));
                                cx.notify();
                            })),
                    )
                    .text_color(cx.theme().muted_foreground),
            )
    }
}
