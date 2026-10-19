//! GPU-independent screen and caller decisions for the game lifecycle.
//!
//! Rendering and world/session ownership stay in the application layer. This
//! module only validates which intent is legal for the current screen and
//! returns a typed decision, so menu behavior can be exercised without a
//! window, renderer, filesystem, or event loop.

/// A visible lifecycle screen. `Playing` is included as the destination for
/// successful create/load actions, but this module does not own a world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleScreen {
    MainMenu,
    NewGame,
    LoadGame,
    Playing,
    Pause,
    Settings(SettingsCaller),
}

/// Compatibility alias for callers that prefer the shorter screen name.
pub type Screen = LifecycleScreen;

/// The screen that opened settings. It determines where Close/Back returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsCaller {
    MainMenu,
    Pause,
}

/// Intents accepted by the main menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainMenuIntent {
    NewGame,
    LoadGame,
    Settings,
    Exit,
    /// More explicit spelling for event-loop integrations.
    ExitGame,
}

/// Intents accepted by the new-game screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewGameIntent {
    Back,
    Create,
}

/// Intents accepted by the load-game screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadGameIntent {
    Back,
    Load,
}

/// Intents accepted by the pause screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseIntent {
    Resume,
    Settings,
    SaveAndQuit,
}

/// Intents accepted by either settings caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsIntent {
    Back,
}

/// A screen-specific intent submitted to [`LifecycleState::dispatch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleIntent {
    MainMenu(MainMenuIntent),
    NewGame(NewGameIntent),
    LoadGame(LoadGameIntent),
    Pause(PauseIntent),
    Settings(SettingsIntent),
}

/// The application-level action resulting from a valid lifecycle intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleDecision {
    /// Move to another screen without starting or saving a world.
    Navigate(LifecycleScreen),
    /// The caller should create a session using the new-game form values.
    StartNewGame,
    /// The caller should load the selected catalog entry.
    LoadWorld,
    /// The caller should return control to the active session.
    ResumeGame,
    /// The caller should save the active session, then show the main menu.
    SaveAndQuit,
    /// The caller should terminate the application.
    ExitGame,
}

/// Returned when an intent is sent to the wrong screen. Keeping this explicit
/// prevents accidental fall-through behavior as more menu screens are added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleError {
    IntentNotAllowed {
        screen: LifecycleScreen,
        intent: LifecycleIntent,
    },
}

pub type LifecycleResult = Result<LifecycleDecision, LifecycleError>;

/// Mutable pure state for the currently visible lifecycle screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleState {
    screen: LifecycleScreen,
}

impl Default for LifecycleState {
    fn default() -> Self {
        Self::new()
    }
}

impl LifecycleState {
    pub const fn new() -> Self {
        Self {
            screen: LifecycleScreen::MainMenu,
        }
    }

    pub const fn from_screen(screen: LifecycleScreen) -> Self {
        Self { screen }
    }

    pub const fn screen(self) -> LifecycleScreen {
        self.screen
    }

    /// Apply a screen-specific intent and update the current screen when the
    /// resulting action has a deterministic destination.
    pub fn dispatch(&mut self, intent: LifecycleIntent) -> LifecycleResult {
        let decision = decide(self.screen, intent)?;
        match decision {
            LifecycleDecision::Navigate(screen) => self.screen = screen,
            LifecycleDecision::StartNewGame | LifecycleDecision::LoadWorld => {
                self.screen = LifecycleScreen::Playing
            }
            LifecycleDecision::ResumeGame => self.screen = LifecycleScreen::Playing,
            LifecycleDecision::SaveAndQuit => self.screen = LifecycleScreen::MainMenu,
            LifecycleDecision::ExitGame => {}
        }
        Ok(decision)
    }
}

/// Pure transition function for callers that do not need to retain state.
pub fn decide(screen: LifecycleScreen, intent: LifecycleIntent) -> LifecycleResult {
    let decision = match (screen, intent) {
        (LifecycleScreen::MainMenu, LifecycleIntent::MainMenu(intent)) => match intent {
            MainMenuIntent::NewGame => LifecycleDecision::Navigate(LifecycleScreen::NewGame),
            MainMenuIntent::LoadGame => LifecycleDecision::Navigate(LifecycleScreen::LoadGame),
            MainMenuIntent::Settings => LifecycleDecision::Navigate(LifecycleScreen::Settings(
                SettingsCaller::MainMenu,
            )),
            MainMenuIntent::Exit | MainMenuIntent::ExitGame => LifecycleDecision::ExitGame,
        },
        (LifecycleScreen::NewGame, LifecycleIntent::NewGame(intent)) => match intent {
            NewGameIntent::Back => LifecycleDecision::Navigate(LifecycleScreen::MainMenu),
            NewGameIntent::Create => LifecycleDecision::StartNewGame,
        },
        (LifecycleScreen::LoadGame, LifecycleIntent::LoadGame(intent)) => match intent {
            LoadGameIntent::Back => LifecycleDecision::Navigate(LifecycleScreen::MainMenu),
            LoadGameIntent::Load => LifecycleDecision::LoadWorld,
        },
        (LifecycleScreen::Pause, LifecycleIntent::Pause(intent)) => match intent {
            PauseIntent::Resume => LifecycleDecision::ResumeGame,
            PauseIntent::Settings => {
                LifecycleDecision::Navigate(LifecycleScreen::Settings(SettingsCaller::Pause))
            }
            PauseIntent::SaveAndQuit => LifecycleDecision::SaveAndQuit,
        },
        (LifecycleScreen::Settings(caller), LifecycleIntent::Settings(SettingsIntent::Back)) => {
            LifecycleDecision::Navigate(match caller {
                SettingsCaller::MainMenu => LifecycleScreen::MainMenu,
                SettingsCaller::Pause => LifecycleScreen::Pause,
            })
        }
        _ => {
            return Err(LifecycleError::IntentNotAllowed { screen, intent });
        }
    };
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_menu_routes_to_every_entry_and_exit() {
        assert_eq!(
            decide(
                Screen::MainMenu,
                LifecycleIntent::MainMenu(MainMenuIntent::NewGame)
            ),
            Ok(LifecycleDecision::Navigate(Screen::NewGame))
        );
        assert_eq!(
            decide(
                Screen::MainMenu,
                LifecycleIntent::MainMenu(MainMenuIntent::LoadGame)
            ),
            Ok(LifecycleDecision::Navigate(Screen::LoadGame))
        );
        assert_eq!(
            decide(
                Screen::MainMenu,
                LifecycleIntent::MainMenu(MainMenuIntent::Settings)
            ),
            Ok(LifecycleDecision::Navigate(Screen::Settings(
                SettingsCaller::MainMenu
            )))
        );
        assert_eq!(
            decide(
                Screen::MainMenu,
                LifecycleIntent::MainMenu(MainMenuIntent::ExitGame)
            ),
            Ok(LifecycleDecision::ExitGame)
        );
    }

    #[test]
    fn new_game_and_load_game_back_or_commit_explicitly() {
        assert_eq!(
            decide(
                Screen::NewGame,
                LifecycleIntent::NewGame(NewGameIntent::Back)
            ),
            Ok(LifecycleDecision::Navigate(Screen::MainMenu))
        );
        assert_eq!(
            decide(
                Screen::NewGame,
                LifecycleIntent::NewGame(NewGameIntent::Create)
            ),
            Ok(LifecycleDecision::StartNewGame)
        );
        assert_eq!(
            decide(
                Screen::LoadGame,
                LifecycleIntent::LoadGame(LoadGameIntent::Back)
            ),
            Ok(LifecycleDecision::Navigate(Screen::MainMenu))
        );
        assert_eq!(
            decide(
                Screen::LoadGame,
                LifecycleIntent::LoadGame(LoadGameIntent::Load)
            ),
            Ok(LifecycleDecision::LoadWorld)
        );
    }

    #[test]
    fn pause_exposes_resume_settings_and_save_and_quit() {
        assert_eq!(
            decide(Screen::Pause, LifecycleIntent::Pause(PauseIntent::Resume)),
            Ok(LifecycleDecision::ResumeGame)
        );
        assert_eq!(
            decide(Screen::Pause, LifecycleIntent::Pause(PauseIntent::Settings)),
            Ok(LifecycleDecision::Navigate(Screen::Settings(
                SettingsCaller::Pause
            )))
        );
        assert_eq!(
            decide(
                Screen::Pause,
                LifecycleIntent::Pause(PauseIntent::SaveAndQuit)
            ),
            Ok(LifecycleDecision::SaveAndQuit)
        );
    }

    #[test]
    fn settings_back_returns_to_the_caller_without_changing_settings_semantics() {
        assert_eq!(
            decide(
                Screen::Settings(SettingsCaller::MainMenu),
                LifecycleIntent::Settings(SettingsIntent::Back)
            ),
            Ok(LifecycleDecision::Navigate(Screen::MainMenu))
        );
        assert_eq!(
            decide(
                Screen::Settings(SettingsCaller::Pause),
                LifecycleIntent::Settings(SettingsIntent::Back)
            ),
            Ok(LifecycleDecision::Navigate(Screen::Pause))
        );
    }

    #[test]
    fn state_dispatch_updates_only_after_valid_transitions() {
        let mut state = LifecycleState::new();
        assert_eq!(
            state.dispatch(LifecycleIntent::MainMenu(MainMenuIntent::NewGame)),
            Ok(LifecycleDecision::Navigate(Screen::NewGame))
        );
        assert_eq!(state.screen(), Screen::NewGame);
        assert_eq!(
            state.dispatch(LifecycleIntent::NewGame(NewGameIntent::Create)),
            Ok(LifecycleDecision::StartNewGame)
        );
        assert_eq!(state.screen(), Screen::Playing);
        assert_eq!(
            state.dispatch(LifecycleIntent::MainMenu(MainMenuIntent::LoadGame)),
            Err(LifecycleError::IntentNotAllowed {
                screen: Screen::Playing,
                intent: LifecycleIntent::MainMenu(MainMenuIntent::LoadGame),
            })
        );
        assert_eq!(state.screen(), Screen::Playing);
    }

    #[test]
    fn wrong_screen_intents_are_structured_errors() {
        let error = decide(
            Screen::MainMenu,
            LifecycleIntent::Pause(PauseIntent::Resume),
        )
        .expect_err("pause actions must not be accepted by main menu");
        assert_eq!(
            error,
            LifecycleError::IntentNotAllowed {
                screen: Screen::MainMenu,
                intent: LifecycleIntent::Pause(PauseIntent::Resume),
            }
        );
    }
}
