#![cfg(target_os = "windows")]
#![expect(
    unsafe_code,
    reason = "An owned, non-activating native window exercises the real UIA provider."
)]

use bootty_computer::{Computer, ComputerAccess, ComputerAction, ComputerError, ComputerResult};
use std::{sync::mpsc, thread, time::Duration};
use windows::{
    Win32::{
        Foundation::POINT,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, GetCursorPos, GetForegroundWindow,
            HMENU, HWND_BOTTOM, MSG, PM_REMOVE, PeekMessageW, SWP_NOACTIVATE, SWP_NOMOVE,
            SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, TranslateMessage, WINDOW_EX_STYLE, WS_CHILD,
            WS_EX_NOACTIVATE, WS_POPUP, WS_VISIBLE,
        },
    },
    core::w,
};

struct Fixture {
    id: u32,
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>>,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let (ready, receive) = mpsc::channel();
        let (stop, stopping) = mpsc::channel();
        let worker = thread::spawn(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                // SAFETY: This thread owns every window it creates here, passes static class and
                // title strings, gives MSG out-pointers that live through each call, and destroys
                // the parent once.
                unsafe {
                    let parent = CreateWindowExW(
                        WS_EX_NOACTIVATE,
                        w!("STATIC"),
                        w!("Bootty UIA value fixture"),
                        WS_POPUP,
                        30,
                        30,
                        400,
                        160,
                        None,
                        None,
                        None,
                        None,
                    )?;
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("EDIT"),
                        w!("initial"),
                        WS_CHILD | WS_VISIBLE,
                        10,
                        10,
                        300,
                        30,
                        Some(parent),
                        Some(HMENU(std::ptr::without_provenance_mut(101))),
                        None,
                        None,
                    )?;
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("BUTTON"),
                        w!("Not editable"),
                        WS_CHILD | WS_VISIBLE,
                        10,
                        60,
                        150,
                        30,
                        Some(parent),
                        Some(HMENU(std::ptr::without_provenance_mut(102))),
                        None,
                        None,
                    )?;
                    SetWindowPos(
                        parent,
                        Some(HWND_BOTTOM),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                    )?;
                    ready.send(u32::try_from(parent.0.expose_provenance())?)?;
                    loop {
                        let mut message = MSG::default();
                        while PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE).as_bool() {
                            let _ = TranslateMessage(&raw const message);
                            DispatchMessageW(&raw const message);
                        }
                        match stopping.recv_timeout(Duration::from_millis(10)) {
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            _ => break,
                        }
                    }
                    DestroyWindow(parent)?;
                    Ok(())
                }
            },
        );
        Ok(Self {
            id: receive.recv_timeout(Duration::from_secs(10))?,
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take()
            && !worker.join().is_ok_and(|result| result.is_ok())
        {
            eprintln!("UIA fixture window thread failed during cleanup");
        }
    }
}

#[tokio::test]
async fn native_edit_value_changes_and_clears_without_foreground_or_pointer_changes() {
    // SAFETY: GetForegroundWindow has no preconditions.
    let foreground = unsafe { GetForegroundWindow() };
    let mut pointer = POINT::default();
    // Locked Windows sessions deny cursor queries, but UIA can still edit the owned window.
    // SAFETY: pointer is a valid out-pointer for the call.
    let pointer_observed = unsafe { GetCursorPos(&raw mut pointer) }.is_ok();
    let fixture = Fixture::new().unwrap();
    let computer = Computer::new();
    let access = ComputerAccess::from_user_setting(true);
    let target = computer
        .targets()
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.window_id == fixture.id)
        .unwrap();
    for value in ["Background value 🥟", ""] {
        let ComputerResult::Elements { elements, .. } = computer
            .execute(&access, &target, &ComputerAction::Elements)
            .await
            .unwrap()
        else {
            panic!("elements")
        };
        let edit = elements
            .into_iter()
            .find(|e| e.identifier.as_deref() == Some("101"))
            .unwrap();
        assert_eq!(
            computer
                .execute(
                    &access,
                    &target,
                    &ComputerAction::SetValue {
                        element: Box::new(edit),
                        value: value.into()
                    }
                )
                .await
                .unwrap(),
            ComputerResult::Posted
        );
        let ComputerResult::Elements { elements, .. } = computer
            .execute(&access, &target, &ComputerAction::Elements)
            .await
            .unwrap()
        else {
            panic!("elements")
        };
        let edit = elements
            .iter()
            .find(|e| e.identifier.as_deref() == Some("101"))
            .unwrap();
        assert_eq!(edit.value.as_deref().unwrap_or_default(), value);
        let button = elements
            .into_iter()
            .find(|e| e.identifier.as_deref() == Some("102"))
            .unwrap();
        assert!(matches!(
            computer
                .execute(
                    &access,
                    &target,
                    &ComputerAction::SetValue {
                        element: Box::new(button),
                        value: "ignored".into()
                    }
                )
                .await,
            Err(ComputerError::InvalidAction(_))
        ));
        // SAFETY: GetForegroundWindow has no preconditions.
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        let mut after = POINT::default();
        if pointer_observed {
            // SAFETY: after is a valid out-pointer for the call.
            unsafe { GetCursorPos(&raw mut after) }.unwrap();
            assert_eq!((after.x, after.y), (pointer.x, pointer.y));
        }
    }
}
