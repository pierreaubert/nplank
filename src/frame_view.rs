//! Converts terminal-compatible grids into retained toolkit scenes and input.

use gpui_ui_kit::scene2d::{
    Scene2DBrush, Scene2DColor, Scene2DGrid, Scene2DInput, Scene2DKeyPhase, Scene2DModifier,
    Scene2DNode, Scene2DNodeKind, Scene2DPointerPhase, Scene2DScene, Scene2DTextAlign,
    Scene2DTransform, ScenePoint, SceneRect,
};

// Monospaced text fits these logical cells; contain fitting scales paint and input together.
const CELL_WIDTH: f32 = 8.0;
const CELL_HEIGHT: f32 = 16.0;
// Scene text has no per-node weight. A half-unit second impression gives
// terminal bold a heavier stroke without changing glyphs or cell advances.
const BOLD_OFFSET: f32 = 0.5;

fn color([r, g, b]: [u8; 3]) -> Scene2DColor {
    Scene2DColor::rgb(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    )
}

fn node(id: String, kind: Scene2DNodeKind) -> Scene2DNode {
    Scene2DNode {
        id,
        kind,
        hit_id: None,
        hit_bounds: None,
        clip: None,
        shadow: None,
        semantic: None,
        transform: Scene2DTransform::default(),
        opacity: 1.0,
        transition: None,
    }
}

pub(crate) fn scene(frame: &crate::frame::Snapshot) -> Scene2DScene {
    let mut scene = Scene2DScene::new(SceneRect::new(
        0.0,
        0.0,
        f32::from(frame.w) * CELL_WIDTH,
        f32::from(frame.h) * CELL_HEIGHT,
    ));
    scene.background = Some(Scene2DBrush::solid(color([24, 24, 24])));
    scene.input.continuous = true;
    // Raw scoped key handling retains typed characters, Tab, and editor shortcuts.
    scene.input.keyboard = false;
    scene.grid = Some(Scene2DGrid {
        rows: u32::from(frame.h),
        columns: u32::from(frame.w),
        x: 0.0,
        y: 0.0,
        cell_width: CELL_WIDTH,
        cell_height: CELL_HEIGHT,
        gap: 0.0,
        row_labels: vec![],
        column_labels: vec![],
    });
    for (index, cell) in frame.cells.iter().enumerate() {
        let x = f32::from(cell.x) * CELL_WIDTH;
        let y = f32::from(cell.y) * CELL_HEIGHT;
        if let Some(background) = cell.bg {
            scene.nodes.push(node(
                format!("bg-{index}"),
                Scene2DNodeKind::Rect {
                    rect: SceneRect::new(x, y, CELL_WIDTH, CELL_HEIGHT),
                    fill: Some(Scene2DBrush::solid(color(background))),
                    stroke: None,
                },
            ));
        }
    }
    // Paint backgrounds first so adjacent cells cannot erase wide Unicode glyphs.
    for (index, cell) in frame.cells.iter().enumerate() {
        let x = f32::from(cell.x) * CELL_WIDTH;
        let y = f32::from(cell.y) * CELL_HEIGHT;
        scene.nodes.push(node(
            format!("glyph-{index}"),
            Scene2DNodeKind::Text {
                origin: ScenePoint::new(x, y),
                content: cell.ch.to_string(),
                size: 12.0,
                color: color(cell.fg),
                font: Some("monospace".into()),
                align: Scene2DTextAlign::Left,
            },
        ));
        if cell.bold {
            scene.nodes.push(node(
                format!("bold-{index}"),
                Scene2DNodeKind::Text {
                    origin: ScenePoint::new(x + BOLD_OFFSET, y),
                    content: cell.ch.to_string(),
                    size: 12.0,
                    color: color(cell.fg),
                    font: Some("monospace".into()),
                    align: Scene2DTextAlign::Left,
                },
            ));
        }
    }
    scene
}

pub(crate) fn input(
    frame: &crate::frame::Snapshot,
    event: Scene2DInput,
) -> Option<crate::frame::Input> {
    use crate::frame::Input;
    match event {
        Scene2DInput::Key {
            phase: Scene2DKeyPhase::Down,
            key,
            modifiers,
            ..
        } => {
            if modifiers.contains(&Scene2DModifier::Meta) {
                return None;
            }
            let key = key.to_lowercase();
            let shift = modifiers.contains(&Scene2DModifier::Shift);
            let ctrl = modifiers.contains(&Scene2DModifier::Control);
            let alt = modifiers.contains(&Scene2DModifier::Alt);
            let mut chars = key.chars();
            let character = chars.next().filter(|_| chars.next().is_none());
            let base = match key.as_str() {
                "space" => " ".into(),
                "return" => "enter".into(),
                "arrowleft" => "left".into(),
                "arrowright" => "right".into(),
                "arrowup" => "up".into(),
                "arrowdown" => "down".into(),
                _ => key.to_lowercase(),
            };
            let mut code = String::new();
            if ctrl {
                code.push_str("ctrl-");
            }
            if alt {
                code.push_str("alt-");
            }
            if shift && character.is_none() && key != "space" {
                code.push_str("shift-");
            }
            code.push_str(&base);
            let text = if ctrl || alt {
                None
            } else if key == "space" {
                Some(' ')
            } else {
                character.map(|c| {
                    if shift {
                        c.to_uppercase().next().unwrap_or(c)
                    } else {
                        c
                    }
                })
            };
            Some(Input::Key { code, text })
        }
        Scene2DInput::Pointer {
            phase,
            position,
            buttons,
            ..
        } => {
            if !position.x.is_finite() || !position.y.is_finite() {
                return None;
            }
            let x = (position.x / CELL_WIDTH)
                .floor()
                .clamp(0.0, f32::from(frame.w.saturating_sub(1))) as u16;
            let y = (position.y / CELL_HEIGHT)
                .floor()
                .clamp(0.0, f32::from(frame.h.saturating_sub(1))) as u16;
            let kind = match phase {
                Scene2DPointerPhase::Down => "down",
                Scene2DPointerPhase::Up | Scene2DPointerPhase::Cancel => "up",
                Scene2DPointerPhase::Move if !buttons.is_empty() => "drag",
                Scene2DPointerPhase::Move => "move",
            };
            Some(Input::Mouse {
                kind: kind.into(),
                x,
                y,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graphical_frame_bold_preserves_unicode_cell_positions_and_background_order() {
        let frame = crate::frame::Snapshot {
            id: "bold-frame".into(),
            component: "editor".into(),
            w: 3,
            h: 1,
            cells: vec![
                crate::frame::Cell {
                    x: 0,
                    y: 0,
                    ch: '界',
                    fg: [12, 34, 56],
                    bg: Some([7, 8, 9]),
                    bold: true,
                },
                crate::frame::Cell {
                    x: 2,
                    y: 0,
                    ch: 'a',
                    fg: [90, 80, 70],
                    bg: Some([1, 2, 3]),
                    bold: false,
                },
            ],
        };
        let scene = scene(&frame);
        assert!(scene.validate().is_ok());
        assert_eq!(scene.nodes.len(), 5);
        assert!(
            scene.nodes[..2]
                .iter()
                .all(|node| matches!(node.kind, Scene2DNodeKind::Rect { .. }))
        );
        let Scene2DNodeKind::Text {
            origin,
            content,
            color: foreground,
            font,
            ..
        } = &scene.nodes[2].kind
        else {
            panic!("missing base glyph")
        };
        let Scene2DNodeKind::Text {
            origin: bold_origin,
            content: bold_content,
            color: bold_color,
            font: bold_font,
            ..
        } = &scene.nodes[3].kind
        else {
            panic!("missing bold impression")
        };
        assert_eq!(content, "界");
        assert_eq!(bold_content, content);
        assert_eq!(bold_color, foreground);
        assert_eq!(bold_font, font);
        assert_eq!(bold_origin.x - origin.x, BOLD_OFFSET);
        assert_eq!(bold_origin.y, origin.y);
        assert!(
            matches!(&scene.nodes[4].kind, Scene2DNodeKind::Text { origin, content, .. } if origin.x == 2.0 * CELL_WIDTH && content == "a")
        );
        assert_eq!(scene.grid.unwrap().cell_width, CELL_WIDTH);
        assert_eq!(
            input(
                &frame,
                Scene2DInput::Pointer {
                    phase: Scene2DPointerPhase::Down,
                    device: gpui_ui_kit::scene2d::Scene2DPointerDevice::Mouse,
                    contact_id: 0,
                    hit_id: None,
                    cell: None,
                    position: ScenePoint::new(2.0 * CELL_WIDTH + 1.0, 1.0),
                    buttons: vec![],
                    modifiers: vec![],
                    timestamp_ns: 0,
                }
            ),
            Some(crate::frame::Input::Mouse {
                kind: "down".into(),
                x: 2,
                y: 0
            })
        );
    }

    #[test]
    fn graphical_frame_scene_preserves_grid_colors_and_keyboard_chords() {
        let frame = crate::frame::Snapshot {
            id: "frame-1".into(),
            component: "test".into(),
            w: 2,
            h: 1,
            cells: vec![crate::frame::Cell {
                x: 1,
                y: 0,
                ch: '🦀',
                fg: [255, 0, 0],
                bg: Some([0, 0, 255]),
                bold: false,
            }],
        };
        let scene = scene(&frame);
        assert!(scene.validate().is_ok());
        assert_eq!(scene.grid.unwrap().columns, 2);
        assert_eq!(scene.nodes.len(), 2);
        assert!(
            matches!(&scene.nodes[1].kind, Scene2DNodeKind::Text { content, color, .. } if content == "🦀" && color.r == 1.0)
        );
        let event = |key: &str, modifiers| Scene2DInput::Key {
            phase: Scene2DKeyPhase::Down,
            key: key.into(),
            modifiers,
            repeat: false,
            timestamp_ns: 0,
        };
        assert_eq!(
            input(&frame, event("a", vec![Scene2DModifier::Shift])),
            Some(crate::frame::Input::Key {
                code: "a".into(),
                text: Some('A')
            })
        );
        assert_eq!(
            input(&frame, event("s", vec![Scene2DModifier::Control])),
            Some(crate::frame::Input::Key {
                code: "ctrl-s".into(),
                text: None
            })
        );
        assert!(input(&frame, event("q", vec![Scene2DModifier::Meta])).is_none());
    }
}
