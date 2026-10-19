//! Reusable, renderer-independent text editing state.
//!
//! The event loop owns keyboard/text event translation and applies the small
//! [`TextInputAction`] vocabulary. This module owns only the edit buffer and
//! cursor, which keeps name/seed fields consistent across menu screens and
//! makes their edge cases straightforward to test.

/// An editing command understood by [`TextInput::apply`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInputAction {
    /// Insert one printable character at the cursor.
    Insert(char),
    /// Insert every printable character in a string, in order.
    InsertText(String),
    MoveLeft,
    MoveRight,
    Home,
    End,
    Backspace,
    Delete,
    Clear,
    Submit,
}

/// The observable result of applying one editing command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInputOutcome {
    /// The command did not change the buffer and did not submit it.
    Unchanged,
    /// The buffer changed; the caller can redraw the field.
    Changed,
    /// The current value was submitted without changing the buffer.
    Submitted(String),
}

/// A bounded text value with a character-indexed insertion cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextInput {
    value: String,
    cursor: usize,
    max_chars: usize,
}

impl TextInput {
    /// Construct an empty field. A zero bound is valid and rejects insertion.
    pub fn new(max_chars: usize) -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            max_chars,
        }
    }

    /// Construct a field from existing text, truncating at a character
    /// boundary when it exceeds `max_chars`.
    pub fn from_text(text: impl AsRef<str>, max_chars: usize) -> Self {
        let mut input = Self::new(max_chars);
        input.insert_text(text.as_ref());
        input
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn len(&self) -> usize {
        self.value.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn max_chars(&self) -> usize {
        self.max_chars
    }

    /// Move the cursor one character to the left.
    pub fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            false
        } else {
            self.cursor -= 1;
            true
        }
    }

    /// Move the cursor one character to the right.
    pub fn move_right(&mut self) -> bool {
        if self.cursor >= self.len() {
            false
        } else {
            self.cursor += 1;
            true
        }
    }

    pub fn move_home(&mut self) -> bool {
        let changed = self.cursor != 0;
        self.cursor = 0;
        changed
    }

    pub fn move_end(&mut self) -> bool {
        let end = self.len();
        let changed = self.cursor != end;
        self.cursor = end;
        changed
    }

    /// Insert one non-control character at the cursor.
    pub fn insert(&mut self, character: char) -> bool {
        if character.is_control() || self.len() >= self.max_chars {
            return false;
        }
        let byte_index = self.cursor_byte_index();
        self.value.insert(byte_index, character);
        self.cursor += 1;
        true
    }

    /// Insert printable characters until the bound is reached. Returns true
    /// when at least one character was accepted.
    pub fn insert_text(&mut self, text: &str) -> bool {
        let mut changed = false;
        for character in text.chars() {
            changed |= self.insert(character);
        }
        changed
    }

    /// Remove the character immediately before the cursor.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        let byte_index = self.cursor_byte_index();
        self.value.remove(byte_index);
        true
    }

    /// Remove the character immediately after the cursor.
    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.len() {
            return false;
        }
        let byte_index = self.cursor_byte_index();
        self.value.remove(byte_index);
        true
    }

    /// Empty the field and place the cursor at the start.
    pub fn clear(&mut self) -> bool {
        if self.value.is_empty() {
            return false;
        }
        self.value.clear();
        self.cursor = 0;
        true
    }

    /// Return a snapshot of the current value for a submit action.
    pub fn submit(&self) -> String {
        self.value.clone()
    }

    /// Apply one keyboard/editing action and return its displayable outcome.
    pub fn apply(&mut self, action: TextInputAction) -> TextInputOutcome {
        match action {
            TextInputAction::Insert(character) => {
                if self.insert(character) {
                    TextInputOutcome::Changed
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            TextInputAction::InsertText(text) => {
                if self.insert_text(&text) {
                    TextInputOutcome::Changed
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            TextInputAction::MoveLeft => Self::outcome(self.move_left()),
            TextInputAction::MoveRight => Self::outcome(self.move_right()),
            TextInputAction::Home => Self::outcome(self.move_home()),
            TextInputAction::End => Self::outcome(self.move_end()),
            TextInputAction::Backspace => Self::outcome(self.backspace()),
            TextInputAction::Delete => Self::outcome(self.delete()),
            TextInputAction::Clear => Self::outcome(self.clear()),
            TextInputAction::Submit => TextInputOutcome::Submitted(self.submit()),
        }
    }

    fn outcome(changed: bool) -> TextInputOutcome {
        if changed {
            TextInputOutcome::Changed
        } else {
            TextInputOutcome::Unchanged
        }
    }

    fn cursor_byte_index(&self) -> usize {
        self.value
            .char_indices()
            .nth(self.cursor)
            .map_or(self.value.len(), |(index, _)| index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_is_bounded_and_cursor_stays_at_accepted_end() {
        let mut input = TextInput::new(3);
        assert_eq!(
            input.apply(TextInputAction::InsertText("ab😀!".into())),
            TextInputOutcome::Changed
        );
        assert_eq!(input.value(), "ab😀");
        assert_eq!(input.len(), 3);
        assert_eq!(input.cursor(), 3);
        assert_eq!(
            input.apply(TextInputAction::Insert('x')),
            TextInputOutcome::Unchanged
        );
    }

    #[test]
    fn cursor_insertion_and_unicode_deletion_use_character_boundaries() {
        let mut input = TextInput::from_text("ab😀", 10);
        assert!(input.move_left());
        assert!(input.move_left());
        assert!(input.insert('X'));
        assert_eq!(input.value(), "aXb😀");
        assert!(input.backspace());
        assert_eq!(input.value(), "ab😀");
        assert!(input.move_right());
        assert!(input.delete());
        assert_eq!(input.value(), "ab");
    }

    #[test]
    fn movement_is_clamped_and_home_end_are_idempotent() {
        let mut input = TextInput::from_text("abc", 10);
        assert!(!input.move_right()); // cursor starts at the end
        assert!(input.move_home());
        assert!(!input.move_left());
        assert!(input.move_right());
        assert!(input.move_end());
        assert!(!input.move_right());
        assert!(!input.move_end());
    }

    #[test]
    fn clear_and_submit_have_explicit_outcomes() {
        let mut input = TextInput::from_text("seed", 10);
        assert_eq!(
            input.apply(TextInputAction::Submit),
            TextInputOutcome::Submitted("seed".into())
        );
        assert_eq!(
            input.apply(TextInputAction::Clear),
            TextInputOutcome::Changed
        );
        assert_eq!(input.value(), "");
        assert_eq!(
            input.apply(TextInputAction::Clear),
            TextInputOutcome::Unchanged
        );
        assert_eq!(
            input.apply(TextInputAction::Submit),
            TextInputOutcome::Submitted(String::new())
        );
    }

    #[test]
    fn control_characters_are_rejected_without_corrupting_the_value() {
        let mut input = TextInput::new(8);
        assert_eq!(
            input.apply(TextInputAction::Insert('\n')),
            TextInputOutcome::Unchanged
        );
        assert_eq!(
            input.apply(TextInputAction::InsertText("ok\r\n".into())),
            TextInputOutcome::Changed
        );
        assert_eq!(input.value(), "ok");
    }
}
