pub mod keybind;

use winit::keyboard::KeyCode;

/// Player keybind preferences, mirroring `InputState` in
/// `src/player/physics.rs`: exactly one physical key per action.
///
/// NOTE: today `src/main.rs` treats `ShiftLeft | ShiftRight` as an equivalent
/// pair for crouch and `ControlLeft | ControlRight` for sprint. The settings
/// model binds exactly one `KeyCode` per action; once S04 wires this in, the
/// un-bound twin stops working. Intentional narrowing, documented here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keybinds {
    pub forward: KeyCode,
    pub backward: KeyCode,
    pub left: KeyCode,
    pub right: KeyCode,
    pub jump_or_up: KeyCode,
    pub crouch_or_down: KeyCode,
    pub sprint: KeyCode,
}

impl Default for Keybinds {
    /// Matches today's hardcoded bindings in `src/main.rs`.
    fn default() -> Self {
        Self {
            forward: KeyCode::KeyW,
            backward: KeyCode::KeyS,
            left: KeyCode::KeyA,
            right: KeyCode::KeyD,
            jump_or_up: KeyCode::Space,
            crouch_or_down: KeyCode::ShiftLeft,
            sprint: KeyCode::ControlLeft,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keybinds_defaults_match_todays_hardcoded_bindings() {
        let d = Keybinds::default();
        assert_eq!(d.forward, KeyCode::KeyW);
        assert_eq!(d.backward, KeyCode::KeyS);
        assert_eq!(d.left, KeyCode::KeyA);
        assert_eq!(d.right, KeyCode::KeyD);
        assert_eq!(d.jump_or_up, KeyCode::Space);
        assert_eq!(d.crouch_or_down, KeyCode::ShiftLeft);
        assert_eq!(d.sprint, KeyCode::ControlLeft);
    }
}
