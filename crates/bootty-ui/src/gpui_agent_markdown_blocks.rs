//! Markdown blocks a reply shows that the text view can't draw by itself: images that are files
//! on the conversation's host, and Mermaid diagrams. Each loads or draws off the UI thread once,
//! then shows where its block stands. Web and data URL images load as the text view loads them.
use super::*;
use crate::presentation::mermaid::DiagramColors;
use bootty_host::files::{FileRequest, FileResponse};
use gpui_kit::component::text::{MarkdownExtensions, MarkdownNode};

/// The most of an image file a reply shows.
// Screenshots and charts fit well inside this; raise it if agents link larger files.
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// A block's picture, as far as it has loaded or drawn.
#[derive(Clone)]
pub(super) enum BlockMedia {
    Loading,
    Ready(gpui_kit::ImageSource),
    Failed(String),
}

/// A picture made off the UI thread, before it becomes an image source there.
enum Picture {
    Bitmap(Arc<gpui_kit::RenderImage>),
    Encoded(Arc<gpui_kit::Image>),
}

impl Picture {
    fn source(self) -> gpui_kit::ImageSource {
        match self {
            Self::Bitmap(image) => gpui_kit::ImageSource::Render(image),
            Self::Encoded(image) => gpui_kit::ImageSource::Image(image),
        }
    }
}

/// An image standing on its own line: its host path and the words it stands for.
struct BlockImage {
    path: String,
    alt: String,
}

/// A finished Mermaid fence's source.
struct Diagram {
    source: String,
}

impl NativeAgentSessionView {
    /// The markdown extensions for the blocks Bootty draws itself.
    pub(super) fn markdown_blocks(&self, owner: gpui_kit::WeakEntity<Self>) -> MarkdownExtensions {
        let cwd = self.record.config.cwd.to_string_lossy().into_owned();
        let diagrams = owner.clone();
        MarkdownExtensions::default()
            // The parser reads only the conversation's directory, which never changes.
            .parser_revision(1)
            .block_parser(move |node, parse| match node {
                markdown::mdast::Node::Paragraph(paragraph) => {
                    let [markdown::mdast::Node::Image(image)] = paragraph.children.as_slice()
                    else {
                        return None;
                    };
                    let path = crate::presentation::markdown_images::markdown_image_path(
                        &image.url, &cwd,
                    )?;
                    let alt = image.alt.clone();
                    Some(
                        MarkdownNode::new("bootty-image", BlockImage { path, alt })
                            .text(image.alt.clone())
                            .markdown(parse.node_source(node).unwrap_or_default()),
                    )
                }
                // A fence still streaming shows as code until its closing line arrives.
                markdown::mdast::Node::Code(code)
                    if code.lang.as_deref() == Some("mermaid")
                        && parse
                            .node_source(node)
                            .is_some_and(|source| closed_fence(source.trim_end())) =>
                {
                    Some(
                        MarkdownNode::new(
                            "bootty-mermaid",
                            Diagram {
                                source: code.value.clone(),
                            },
                        )
                        .text(code.value.clone())
                        .markdown(parse.node_source(node).unwrap_or_default()),
                    )
                }
                _ => None,
            })
            .block_renderer("bootty-image", move |node, _, cx| {
                let Some(image) = node.data::<BlockImage>() else {
                    return div().into_any_element();
                };
                let key = format!("image:{}", image.path);
                let media = block_media(&owner, &key, cx);
                if media.is_none() {
                    let (owner, path) = (owner.clone(), image.path.clone());
                    cx.defer(move |cx| {
                        _ = owner.update(cx, |view, cx| view.load_image(key, path, cx));
                    });
                }
                render_image(image, media, cx)
            })
            .block_renderer("bootty-mermaid", move |node, _, cx| {
                let Some(diagram) = node.data::<Diagram>() else {
                    return div().into_any_element();
                };
                let colors = diagram_colors(cx);
                let key = format!("mermaid:{}:{}", colors.dark, diagram.source);
                let media = block_media(&diagrams, &key, cx);
                if media.is_none() {
                    let (owner, source) = (diagrams.clone(), diagram.source.clone());
                    cx.defer(move |cx| {
                        _ = owner.update(cx, |view, cx| view.draw_diagram(key, source, colors, cx));
                    });
                }
                render_diagram(diagram, media, cx)
            })
    }

    /// Reads and decodes the image at host `path`, once, then shows it.
    fn load_image(&mut self, key: String, path: String, cx: &Context<Self>) {
        let remote = self
            .record
            .config
            .remote
            .as_ref()
            .map(|remote| bootty_host::remote::RemoteHost::new(remote.host.clone()));
        self.load_block(key, cx, move || {
            read_image(&path, remote.as_ref()).map(Picture::Bitmap)
        });
    }

    /// Draws the Mermaid diagram `source`, once, then shows it.
    fn draw_diagram(
        &mut self,
        key: String,
        source: String,
        colors: DiagramColors,
        cx: &Context<Self>,
    ) {
        self.load_block(key, cx, move || {
            let svg = crate::presentation::mermaid::render_mermaid(&source, &colors)?;
            Ok(Picture::Encoded(Arc::new(gpui_kit::Image::from_bytes(
                gpui_kit::ImageFormat::Svg,
                svg.into_bytes(),
            ))))
        });
    }

    /// Makes the picture for `key` with `make` off the UI thread, once, then shows it.
    fn load_block(
        &mut self,
        key: String,
        cx: &Context<Self>,
        make: impl FnOnce() -> Result<Picture, String> + Send + 'static,
    ) {
        if self.markdown_blocks.contains_key(&key) {
            return;
        }
        self.markdown_blocks
            .insert(key.clone(), BlockMedia::Loading);
        cx.spawn(async move |this, cx| {
            let picture = cx.background_executor().spawn(async move { make() }).await;
            _ = this.update(cx, |this, cx| {
                this.markdown_blocks.insert(
                    key,
                    picture.map_or_else(BlockMedia::Failed, |picture| {
                        BlockMedia::Ready(picture.source())
                    }),
                );
                // The rows with the block change height.
                this.list.remeasure();
                cx.notify();
            });
        })
        .detach();
    }
}

/// What's known of the picture for `key`.
fn block_media(
    owner: &gpui_kit::WeakEntity<NativeAgentSessionView>,
    key: &str,
    cx: &App,
) -> Option<BlockMedia> {
    owner
        .read_with(cx, |view, _| view.markdown_blocks.get(key).cloned())
        .ok()
        .flatten()
}

/// Whether a fence's source ends with its closing line.
fn closed_fence(source: &str) -> bool {
    let fence = |line: &str| {
        let line = line.trim_start();
        line.starts_with("```") || line.starts_with("~~~")
    };
    let mut lines = source.lines();
    lines.next().is_some_and(fence) && lines.next_back().is_some_and(fence)
}

/// The theme's colors for diagrams.
fn diagram_colors(cx: &App) -> DiagramColors {
    let theme = cx.theme();
    let hex = |color: gpui_kit::Hsla| {
        let color = gpui_kit::Rgba::from(color);
        let byte = |channel: f32| {
            num_traits::ToPrimitive::to_u8(&(channel.clamp(0., 1.) * 255.).round()).unwrap_or(0)
        };
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(color.r),
            byte(color.g),
            byte(color.b)
        )
    };
    DiagramColors {
        dark: theme.is_dark(),
        font: theme.font_family.to_string(),
        background: hex(theme.background),
        node: hex(theme.muted),
        text: hex(theme.foreground),
        line: hex(theme.muted_foreground),
        border: hex(theme.border),
    }
}

/// Reads an image file on its host and decodes it.
fn read_image(
    path: &str,
    remote: Option<&bootty_host::remote::RemoteHost>,
) -> Result<Arc<gpui_kit::RenderImage>, String> {
    let request = FileRequest::Read {
        path: path.to_owned(),
    };
    let response = remote
        .map_or_else(
            || request.execute(),
            |remote| request.execute_remote(remote, bootty_host::SystemCommandRunner),
        )
        .map_err(|error| format!("Couldn't read it: {error}"))?;
    let FileResponse::Media(file) = response else {
        return Err("It isn't an image".to_owned());
    };
    if file.len > MAX_IMAGE_BYTES {
        return Err("It's larger than 32 MiB".to_owned());
    }
    let source = bootty_host::media::MediaReader::open(&file, remote)
        .map_err(|error| format!("Couldn't read it: {error}"))?;
    crate::gpui_document_panel::decode_image(source)
        .map_err(|error| format!("Couldn't show it: {error}"))
}

/// The image at its own size, at most the reply's width, or what it stands for while it loads or
/// when it can't show.
fn render_image(image: &BlockImage, media: Option<BlockMedia>, cx: &App) -> gpui_kit::AnyElement {
    let selector = format!("markdown-image:{}", image.path);
    if let Some(BlockMedia::Ready(source)) = media {
        return gpui_kit::img(source)
            .debug_selector(move || selector)
            .max_w_full()
            .into_any_element();
    }
    let note = match media {
        Some(BlockMedia::Failed(reason)) => format!("{}: {reason}", image.path),
        _ => image.path.clone(),
    };
    div()
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(crate::gpui::sized_icon(
            "image",
            crate::gpui::IconSize::Small,
            cx.theme().muted_foreground,
        ))
        .child(if image.alt.is_empty() {
            note
        } else {
            format!("{} — {note}", image.alt)
        })
        .into_any_element()
}

/// The drawn diagram, at most the reply's width; its source while it draws, and with the reason
/// when Mermaid can't draw it.
fn render_diagram(diagram: &Diagram, media: Option<BlockMedia>, cx: &App) -> gpui_kit::AnyElement {
    if let Some(BlockMedia::Ready(source)) = media {
        return gpui_kit::img(source)
            .debug_selector(|| "markdown-diagram".into())
            .max_w_full()
            .into_any_element();
    }
    div()
        .debug_selector(|| "markdown-diagram-source".into())
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted)
        .children(match media {
            Some(BlockMedia::Failed(reason)) => {
                Some(div().text_xs().text_color(cx.theme().danger).child(reason))
            }
            _ => None,
        })
        .child(
            div()
                .font_family(cx.theme().mono_font_family.clone())
                .text_sm()
                .whitespace_normal()
                .child(diagram.source.clone()),
        )
        .into_any_element()
}
