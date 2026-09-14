use bootty_config::{
    color::Color,
    config::{SegmentAlign, StatusSegment},
};

/// One typed mutation of an ordered status-segment list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusSegmentEdit {
    Add {
        module: String,
    },
    Remove {
        index: usize,
    },
    Move {
        index: usize,
        offset: isize,
    },
    SetModule {
        index: usize,
        module: String,
    },
    SetAlignment {
        index: usize,
        alignment: SegmentAlign,
    },
    SetForeground {
        index: usize,
        color: Option<Color>,
    },
    SetBackground {
        index: usize,
        color: Option<Color>,
    },
    SetIcon {
        index: usize,
        icon: Option<String>,
    },
}

pub fn apply_status_segment_edit(
    segments: &mut Vec<StatusSegment>,
    edit: StatusSegmentEdit,
) -> Result<(), String> {
    match edit {
        StatusSegmentEdit::Add { module } => {
            let module = required_module(module)?;
            segments.push(StatusSegment {
                module,
                ..StatusSegment::default()
            });
        }
        StatusSegmentEdit::Remove { index } => {
            if index >= segments.len() {
                return Err(missing_segment(index));
            }
            segments.remove(index);
        }
        StatusSegmentEdit::Move { index, offset } => {
            let Some(target) = index.checked_add_signed(offset) else {
                return Err(missing_segment(index));
            };
            if index >= segments.len() || target >= segments.len() {
                return Err(missing_segment(index));
            }
            let segment = segments.remove(index);
            segments.insert(target, segment);
        }
        StatusSegmentEdit::SetModule { index, module } => {
            segment_at(segments, index)?.module = required_module(module)?;
        }
        StatusSegmentEdit::SetAlignment { index, alignment } => {
            segment_at(segments, index)?.align = alignment;
        }
        StatusSegmentEdit::SetForeground { index, color } => {
            segment_at(segments, index)?.fg = color;
        }
        StatusSegmentEdit::SetBackground { index, color } => {
            segment_at(segments, index)?.bg = color;
        }
        StatusSegmentEdit::SetIcon { index, icon } => {
            segment_at(segments, index)?.icon = icon.filter(|icon| !icon.is_empty());
        }
    }
    Ok(())
}

fn segment_at(segments: &mut [StatusSegment], index: usize) -> Result<&mut StatusSegment, String> {
    segments
        .get_mut(index)
        .ok_or_else(|| missing_segment(index))
}

fn required_module(module: String) -> Result<String, String> {
    (!module.trim().is_empty())
        .then_some(module)
        .ok_or_else(|| "Status module name cannot be empty.".to_owned())
}

fn missing_segment(index: usize) -> String {
    format!("Status segment {index} no longer exists.")
}
