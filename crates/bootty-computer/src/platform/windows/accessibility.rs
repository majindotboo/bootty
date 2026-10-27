//! UIA objects stay on the blocking worker's MTA, including their final release.
use std::time::{Duration, Instant};
use windows::Win32::UI::Accessibility as uia;

use windows::{
    Win32::{
        Foundation::{E_NOINTERFACE, E_POINTER, S_OK},
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::Accessibility::{
            CUIAutomation8, IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationElement,
            IUIAutomationInvokePattern, IUIAutomationSelectionItemPattern,
            IUIAutomationTextPattern, IUIAutomationTogglePattern, IUIAutomationTreeWalker,
            IUIAutomationValuePattern, ToggleState_On, TreeScope_Element,
            UIA_AutomationIdPropertyId, UIA_BoundingRectanglePropertyId, UIA_CONTROLTYPE_ID,
            UIA_ControlTypePropertyId, UIA_DocumentControlTypeId, UIA_E_ELEMENTNOTAVAILABLE,
            UIA_E_NOTSUPPORTED, UIA_EditControlTypeId, UIA_HasKeyboardFocusPropertyId,
            UIA_InvokePatternId, UIA_IsEnabledPropertyId, UIA_IsOffscreenPropertyId,
            UIA_IsPasswordPropertyId, UIA_NamePropertyId, UIA_PATTERN_ID,
            UIA_SelectionItemPatternId, UIA_TextPatternId, UIA_TogglePatternId, UIA_ValuePatternId,
        },
    },
    core::{BSTR, Interface},
};

use crate::{
    ComputerElement, ComputerError, DisplayBounds, ElementRoot, ElementState, MAX_ELEMENT_DEPTH,
    desktop::NativeWindow,
};

const MAX_READS: usize = 4_000;
const BUDGET: Duration = Duration::from_secs(3);
const MAX_TEXT: usize = 200;

struct Apartment;
impl Apartment {
    fn new() -> Result<Self, ComputerError> {
        // SAFETY: CoInitializeEx joins this thread to the MTA; Drop balances it with
        // CoUninitialize.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: Apartment::new succeeded on this thread, so this balances exactly one
        // CoInitializeEx.
        unsafe { CoUninitialize() };
    }
}

struct Session {
    root: IUIAutomationElement,
    walker: IUIAutomationTreeWalker,
    cache: IUIAutomationCacheRequest,
    _automation: IUIAutomation2,
    // Release all COM interfaces before uninitializing their apartment.
    _apartment: Apartment,
}

impl Session {
    fn new(window: &NativeWindow) -> Result<Self, ComputerError> {
        let apartment = Apartment::new()?;
        // SAFETY: COM is initialized on this thread, and CUIAutomation8 is a registered in-process
        // class.
        let automation: IUIAutomation2 =
            unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }?;
        // SAFETY: automation is a live UIA client in this apartment, and each setting is a plain
        // value.
        unsafe { automation.SetAutoSetFocus(false) }?;
        // SAFETY: As above.
        unsafe { automation.SetConnectionTimeout(500) }?;
        // SAFETY: As above.
        unsafe { automation.SetTransactionTimeout(500) }?;
        // SAFETY: window.id is a window handle; UIA rejects a stale one instead of reading it.
        let root = unsafe { automation.ElementFromHandle(super::hwnd(window.id)) }?;
        // SAFETY: automation is a live UIA client in this apartment.
        let walker = unsafe { automation.ControlViewWalker() }?;
        // SAFETY: automation is a live UIA client in this apartment.
        let cache = unsafe { automation.CreateCacheRequest() }?;
        // SAFETY: cache is a live cache request, and the scope is a plain value.
        unsafe { cache.SetTreeScope(TreeScope_Element) }?;
        for property in [
            UIA_ControlTypePropertyId,
            UIA_NamePropertyId,
            UIA_AutomationIdPropertyId,
            UIA_BoundingRectanglePropertyId,
            UIA_IsEnabledPropertyId,
            UIA_HasKeyboardFocusPropertyId,
            UIA_IsPasswordPropertyId,
            UIA_IsOffscreenPropertyId,
        ] {
            // SAFETY: cache is a live cache request, and property is a UIA property id.
            unsafe { cache.AddProperty(property) }?;
        }
        Ok(Self {
            root,
            walker,
            cache,
            _automation: automation,
            _apartment: apartment,
        })
    }

    fn first(
        &self,
        parent: &IUIAutomationElement,
    ) -> Result<Option<IUIAutomationElement>, ComputerError> {
        // SAFETY: walker, parent and the cache are live UIA objects in this apartment.
        optional_element(unsafe {
            self.walker
                .GetFirstChildElementBuildCache(parent, &self.cache)
        })
    }

    fn next(
        &self,
        element: &IUIAutomationElement,
    ) -> Result<Option<IUIAutomationElement>, ComputerError> {
        // SAFETY: walker, element and the cache are live UIA objects in this apartment.
        optional_element(unsafe {
            self.walker
                .GetNextSiblingElementBuildCache(element, &self.cache)
        })
    }

    fn resolve(&self, path: &[u16]) -> Result<Option<IUIAutomationElement>, ComputerError> {
        let started = Instant::now();
        let mut parent = self.root.clone();
        let mut reads = 0_usize;
        for index in path {
            if started.elapsed() >= BUDGET || reads >= MAX_READS {
                return Err(ComputerError::Timeout);
            }
            reads = reads.saturating_add(1);
            let mut child = self.first(&parent)?;
            for _ in 0..*index {
                let Some(current) = child else {
                    return Ok(None);
                };
                if started.elapsed() >= BUDGET || reads >= MAX_READS {
                    return Err(ComputerError::Timeout);
                }
                reads = reads.saturating_add(1);
                child = self.next(&current)?;
            }
            let Some(child) = child else { return Ok(None) };
            parent = child;
        }
        Ok(Some(parent))
    }
}

struct Listing<'a> {
    session: &'a Session,
    window: &'a NativeWindow,
    started: Instant,
    reads: usize,
    limit: usize,
    elements: Vec<ComputerElement>,
    omitted: bool,
}

impl Listing<'_> {
    fn visit(&mut self, parent: &IUIAutomationElement, path: &[u16]) -> Result<(), ComputerError> {
        if path.len() >= MAX_ELEMENT_DEPTH {
            self.omitted = true;
            return Ok(());
        }
        let mut child = self.session.first(parent)?;
        let mut index = 0_u16;
        while let Some(element) = child {
            if self.started.elapsed() >= BUDGET
                || self.reads >= MAX_READS
                || self.elements.len() >= self.limit
            {
                self.omitted = true;
                return Ok(());
            }
            self.reads = self.reads.saturating_add(1);
            let mut child_path = path.to_vec();
            child_path.push(index);
            if let Some(observed) = describe(&element, child_path.clone(), self.window)? {
                self.elements.push(observed);
            }
            self.visit(&element, &child_path)?;
            if self.started.elapsed() >= BUDGET
                || self.reads >= MAX_READS
                || self.elements.len() >= self.limit
            {
                self.omitted = true;
                return Ok(());
            }
            child = self.session.next(&element)?;
            let Some(next) = index.checked_add(1) else {
                self.omitted |= child.is_some();
                return Ok(());
            };
            index = next;
        }
        Ok(())
    }
}

pub(super) fn elements(
    window: &NativeWindow,
    limit: usize,
) -> Result<(Vec<ComputerElement>, bool), ComputerError> {
    let session = Session::new(window)?;
    let mut listing = Listing {
        session: &session,
        window,
        started: Instant::now(),
        reads: 0,
        limit,
        elements: Vec::new(),
        omitted: false,
    };
    listing.visit(&session.root, &[])?;
    // Every window element has a frame.
    let origin = |element: &ComputerElement| {
        element
            .frame
            .as_ref()
            .map_or((f64::INFINITY, f64::INFINITY), |frame| (frame.y, frame.x))
    };
    listing.elements.sort_by(|a, b| {
        let ((ay, ax), (by, bx)) = (origin(a), origin(b));
        ay.total_cmp(&by)
            .then_with(|| ax.total_cmp(&bx))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok((listing.elements, listing.omitted))
}

pub(super) fn element(
    window: &NativeWindow,
    path: &[u16],
) -> Result<Option<ComputerElement>, ComputerError> {
    let session = Session::new(window)?;
    session
        .resolve(path)?
        .map(|element| describe(&element, path.to_vec(), window))
        .transpose()
        .map(Option::flatten)
}

pub(super) fn press(window: &NativeWindow, path: &[u16]) -> Result<bool, ComputerError> {
    let session = Session::new(window)?;
    let element = session.resolve(path)?.ok_or(ComputerError::StaleElement)?;
    // SAFETY: element is a live UIA element in this session's apartment.
    if !unsafe { element.CurrentIsEnabled() }?.as_bool() {
        return Err(ComputerError::InvalidAction(
            "the element is disabled".into(),
        ));
    }
    if let Some(invoke) = pattern::<IUIAutomationInvokePattern>(&element, UIA_InvokePatternId)? {
        // SAFETY: invoke is a live pattern of an enabled element.
        unsafe { invoke.Invoke() }?;
    } else if let Some(toggle) =
        pattern::<IUIAutomationTogglePattern>(&element, UIA_TogglePatternId)?
    {
        // SAFETY: toggle is a live pattern of an enabled element.
        unsafe { toggle.Toggle() }?;
    } else if let Some(selection) =
        pattern::<IUIAutomationSelectionItemPattern>(&element, UIA_SelectionItemPatternId)?
    {
        // SAFETY: selection is a live pattern of an enabled element.
        unsafe { selection.Select() }?;
    } else {
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn set_value(
    window: &NativeWindow,
    path: &[u16],
    value: &str,
) -> Result<(), ComputerError> {
    let session = Session::new(window)?;
    let element = session.resolve(path)?.ok_or(ComputerError::StaleElement)?;
    // SAFETY: element is a live UIA element in this session's apartment.
    if !unsafe { element.CurrentIsEnabled() }?.as_bool() {
        return Err(ComputerError::InvalidAction(
            "the element is disabled".into(),
        ));
    }
    let pattern =
        pattern::<IUIAutomationValuePattern>(&element, UIA_ValuePatternId)?.ok_or_else(|| {
            ComputerError::InvalidAction("the element has no writable accessibility value".into())
        })?;
    // SAFETY: pattern is a live value pattern of an enabled element.
    if unsafe { pattern.CurrentIsReadOnly() }?.as_bool() {
        return Err(ComputerError::InvalidAction(
            "the element is read-only".into(),
        ));
    }
    // SAFETY: pattern is a live, writable value pattern, and the BSTR outlives the call.
    unsafe { pattern.SetValue(&BSTR::from(value)) }.map_err(ComputerError::from)
}

fn describe(
    element: &IUIAutomationElement,
    path: Vec<u16>,
    window: &NativeWindow,
) -> Result<Option<ComputerElement>, ComputerError> {
    // SAFETY: element was built with this property in its cache.
    if unsafe { element.CachedIsOffscreen() }?.as_bool() {
        return Ok(None);
    }
    // SAFETY: element was built with this property in its cache.
    let rect = unsafe { element.CachedBoundingRectangle() }?;
    let frame = DisplayBounds {
        x: f64::from(rect.left),
        y: f64::from(rect.top),
        width: f64::from(rect.right.saturating_sub(rect.left)),
        height: f64::from(rect.bottom.saturating_sub(rect.top)),
    };
    if frame.width <= 0.0
        || frame.height <= 0.0
        || !contains(
            &window.bounds,
            frame.x + frame.width / 2.0,
            frame.y + frame.height / 2.0,
        )
    {
        return Ok(None);
    }
    // SAFETY: element was built with this property in its cache.
    let control_type = unsafe { element.CachedControlType() }?;
    // SAFETY: element was built with this property in its cache.
    let name = text(&unsafe { element.CachedName() }?);
    // SAFETY: element was built with this property in its cache.
    let identifier = text(&unsafe { element.CachedAutomationId() }?);
    let mut states = Vec::new();
    // SAFETY: element was built with this property in its cache.
    if !unsafe { element.CachedIsEnabled() }?.as_bool() {
        states.push(ElementState::Disabled);
    }
    // SAFETY: element was built with this property in its cache.
    if unsafe { element.CachedHasKeyboardFocus() }?.as_bool() {
        states.push(ElementState::Focused);
    }
    if let Some(selection) =
        pattern::<IUIAutomationSelectionItemPattern>(element, UIA_SelectionItemPatternId)?
        // SAFETY: selection is a live pattern of this element.
        && unsafe { selection.CurrentIsSelected() }?.as_bool()
    {
        states.push(ElementState::Selected);
    }
    if let Some(toggle) = pattern::<IUIAutomationTogglePattern>(element, UIA_TogglePatternId)?
        // SAFETY: toggle is a live pattern of this element.
        && unsafe { toggle.CurrentToggleState() }? == ToggleState_On
    {
        states.push(ElementState::Checked);
    }
    // SAFETY: element was built with this property in its cache.
    let value = if unsafe { element.CachedIsPassword() }?.as_bool() {
        None
    } else if let Some(value) = pattern::<IUIAutomationValuePattern>(element, UIA_ValuePatternId)? {
        // SAFETY: value is a live value pattern of this element.
        text(&unsafe { value.CurrentValue() }?)
    } else if (control_type == UIA_DocumentControlTypeId || control_type == UIA_EditControlTypeId)
        && let Some(pattern) = pattern::<IUIAutomationTextPattern>(element, UIA_TextPatternId)?
    {
        // SAFETY: pattern is a live text pattern of this element.
        let range = unsafe { pattern.DocumentRange() }?;
        // SAFETY: range is a live text range, and the limit bounds the copy.
        text(&unsafe { range.GetText(200) }?)
    } else {
        None
    };
    Ok(Some(ComputerElement {
        path,
        root: ElementRoot::Window,
        role: role(control_type),
        name,
        value,
        identifier,
        frame: Some(frame),
        states,
    }))
}

fn pattern<T: Interface>(
    element: &IUIAutomationElement,
    id: UIA_PATTERN_ID,
) -> Result<Option<T>, ComputerError> {
    // SAFETY: element is a live UIA element, and T is the interface that pattern id returns.
    match unsafe { element.GetCurrentPatternAs(id) } {
        Ok(pattern) => Ok(Some(pattern)),
        Err(error)
            if matches!(error.code(), E_NOINTERFACE | E_POINTER | S_OK)
                || error.code().0 == i32::from_ne_bytes(UIA_E_NOTSUPPORTED.to_ne_bytes()) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn optional_element(
    result: windows::core::Result<IUIAutomationElement>,
) -> Result<Option<IUIAutomationElement>, ComputerError> {
    match result {
        Ok(element) => Ok(Some(element)),
        // UIA returns S_OK plus a null interface at the end of a walk. windows-rs represents
        // that nullable COM result as Error::empty(), whose HRESULT is also S_OK.
        Err(error) if matches!(error.code(), E_POINTER | S_OK) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// An element that went away is stale; every other UI Automation failure is the platform's.
impl From<windows::core::Error> for ComputerError {
    fn from(error: windows::core::Error) -> Self {
        if error.code().0 == i32::from_ne_bytes(UIA_E_ELEMENTNOTAVAILABLE.to_ne_bytes()) {
            Self::StaleElement
        } else {
            Self::Platform(format!("Windows UI Automation: {error}"))
        }
    }
}

fn text(value: &BSTR) -> Option<String> {
    let value: String = value.to_string().chars().take(MAX_TEXT).collect();
    (!value.is_empty()).then_some(value)
}

fn contains(frame: &DisplayBounds, x: f64, y: f64) -> bool {
    x >= frame.x && y >= frame.y && x < frame.x + frame.width && y < frame.y + frame.height
}

fn role(control_type: UIA_CONTROLTYPE_ID) -> String {
    match control_type {
        uia::UIA_ButtonControlTypeId => "Button",
        uia::UIA_CheckBoxControlTypeId => "CheckBox",
        uia::UIA_ComboBoxControlTypeId => "ComboBox",
        uia::UIA_EditControlTypeId => "Edit",
        uia::UIA_HyperlinkControlTypeId => "Link",
        uia::UIA_ImageControlTypeId => "Image",
        uia::UIA_ListItemControlTypeId => "ListItem",
        uia::UIA_ListControlTypeId => "List",
        uia::UIA_MenuControlTypeId => "Menu",
        uia::UIA_MenuBarControlTypeId => "MenuBar",
        uia::UIA_MenuItemControlTypeId => "MenuItem",
        uia::UIA_ProgressBarControlTypeId => "ProgressBar",
        uia::UIA_RadioButtonControlTypeId => "RadioButton",
        uia::UIA_ScrollBarControlTypeId => "ScrollBar",
        uia::UIA_SliderControlTypeId => "Slider",
        uia::UIA_SpinnerControlTypeId => "Spinner",
        uia::UIA_StatusBarControlTypeId => "StatusBar",
        uia::UIA_TabControlTypeId => "Tab",
        uia::UIA_TabItemControlTypeId => "TabItem",
        uia::UIA_TextControlTypeId => "Text",
        uia::UIA_ToolBarControlTypeId => "ToolBar",
        uia::UIA_ToolTipControlTypeId => "ToolTip",
        uia::UIA_TreeControlTypeId => "Tree",
        uia::UIA_TreeItemControlTypeId => "TreeItem",
        uia::UIA_CustomControlTypeId => "Custom",
        uia::UIA_GroupControlTypeId => "Group",
        uia::UIA_DataGridControlTypeId => "DataGrid",
        uia::UIA_DataItemControlTypeId => "DataItem",
        uia::UIA_DocumentControlTypeId => "Document",
        uia::UIA_WindowControlTypeId => "Window",
        uia::UIA_PaneControlTypeId => "Pane",
        uia::UIA_HeaderControlTypeId => "Header",
        uia::UIA_HeaderItemControlTypeId => "HeaderItem",
        uia::UIA_TableControlTypeId => "Table",
        uia::UIA_TitleBarControlTypeId => "TitleBar",
        uia::UIA_SeparatorControlTypeId => "Separator",
        _ => return format!("ControlType({})", control_type.0),
    }
    .to_owned()
}
