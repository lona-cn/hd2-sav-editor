//! Draft editing over an immutable disk snapshot, with undo/redo.
//!
//! Three objects stay distinct:
//! * [`Snapshot`] — the disk state the draft is based on (never mutated here);
//! * [`Draft`] — the user's pending intent plus its history;
//! * the committed result — produced by the save transaction, not by this type.

use crate::types::{ItemRef, SlotIntent};

/// Payload offsets used by the draft. Mirrors `sav_codec::fields` without
/// depending on the codec crate for plain arithmetic.
pub const OFFSET_HEAD: usize = 0x0121;
pub const OFFSET_BODY: usize = 0x0129;
pub const OFFSET_CAPE: usize = 0x0125;

/// An immutable view of one decoded save.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub raw: Vec<u8>,
    pub payload: Vec<u8>,
    pub sha256: String,
    pub captured_at: String,
    pub path: String,
    /// Whether the verified layout matched, i.e. writes are allowed.
    pub writable: bool,
    /// Reason the layout is read-only, when it is.
    pub readonly_reason: Option<String>,
    pub outer_crc32: u32,
    pub inner_low32: u32,
}

impl Snapshot {
    /// Read a u32 field from the payload.
    pub fn read_u32(&self, offset: usize) -> Option<u32> {
        let end = offset.checked_add(4)?;
        let slice = self.payload.get(offset..end)?;
        Some(u32::from_le_bytes(slice.try_into().unwrap()))
    }

    /// Currently equipped head ID.
    pub fn head_id(&self) -> Option<u32> {
        self.read_u32(OFFSET_HEAD)
    }

    /// Currently equipped body armor ID.
    pub fn body_id(&self) -> Option<u32> {
        self.read_u32(OFFSET_BODY)
    }

    /// Currently equipped cape ID.
    pub fn cape_id(&self) -> Option<u32> {
        self.read_u32(OFFSET_CAPE)
    }

    /// Short revision identifier derived from the raw SHA256.
    pub fn revision(&self) -> String {
        self.sha256.chars().take(12).collect()
    }
}

/// The user's pending intent, including the body lock.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadoutIntent {
    pub head: SlotIntent,
    pub body: SlotIntent,
}

impl Default for LoadoutIntent {
    fn default() -> Self {
        LoadoutIntent {
            head: SlotIntent::Keep,
            body: SlotIntent::Keep,
        }
    }
}

impl LoadoutIntent {
    /// Whether either slot would be written.
    pub fn is_dirty(&self) -> bool {
        self.head.is_set() || self.body.is_set()
    }

    /// Type of the item targeted at the head slot, when one is set.
    pub fn head_item_type(&self) -> Option<crate::types::ItemType> {
        match &self.head {
            SlotIntent::Keep => None,
            SlotIntent::Set { item } => Some(item.item_type),
        }
    }

    /// Type of the item targeted at the body slot, when one is set.
    pub fn body_item_type(&self) -> Option<crate::types::ItemType> {
        match &self.body {
            SlotIntent::Keep => None,
            SlotIntent::Set { item } => Some(item.item_type),
        }
    }
}

/// One reversible user action. Applying a preset pair is a single command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Replace the whole intent (used by "apply preset" and "reset draft").
    /// Boxed: two intents inline make this variant dominate the enum's size.
    SetIntent {
        label: String,
        before: Box<LoadoutIntent>,
        after: Box<LoadoutIntent>,
    },
    /// Change the body lock and any body intent cleared by re-locking.
    SetBodyLock {
        before: bool,
        after: bool,
        before_intent: Box<LoadoutIntent>,
        after_intent: Box<LoadoutIntent>,
    },
}

impl Command {
    /// Label shown in the undo tooltip.
    pub fn label(&self) -> &str {
        match self {
            Command::SetIntent { label, .. } => label,
            Command::SetBodyLock { after: true, .. } => "锁定身体",
            Command::SetBodyLock { after: false, .. } => "解除身体锁定",
        }
    }
}

/// The draft state machine.
#[derive(Debug, Clone)]
pub struct Draft {
    /// The snapshot this draft is based on. Never replaced by an external change
    /// without an explicit user decision.
    base: Snapshot,
    intent: LoadoutIntent,
    body_locked: bool,
    undo_stack: Vec<Command>,
    redo_stack: Vec<Command>,
    /// When set, the disk changed underneath a dirty draft.
    conflict: Option<ConflictInfo>,
    /// Head ID recorded before this tool turned the head slot into armor, when known.
    recorded_helmet: Option<ItemRef>,
}

/// Details of an unresolved conflict between the draft and the disk.
#[derive(Debug, Clone, PartialEq)]
pub struct ConflictInfo {
    pub base_revision: String,
    pub disk_revision: String,
    pub detected_at: String,
    pub disk_head_id: Option<u32>,
    pub disk_body_id: Option<u32>,
}

impl Draft {
    /// Start a draft with both armor slots editable.
    pub fn new(base: Snapshot) -> Draft {
        Draft {
            base,
            intent: LoadoutIntent::default(),
            body_locked: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            conflict: None,
            recorded_helmet: None,
        }
    }

    /// The snapshot this draft is based on.
    pub fn base(&self) -> &Snapshot {
        &self.base
    }

    /// The current intent.
    pub fn intent(&self) -> &LoadoutIntent {
        &self.intent
    }

    /// Whether the body slot is locked against edits.
    pub fn body_locked(&self) -> bool {
        self.body_locked
    }

    /// Whether the draft would write anything.
    pub fn is_dirty(&self) -> bool {
        self.intent.is_dirty()
    }

    /// Whether an external change is pending resolution.
    pub fn conflict(&self) -> Option<&ConflictInfo> {
        self.conflict.as_ref()
    }

    /// The helmet recorded before this tool introduced a dual-armor state.
    pub fn recorded_helmet(&self) -> Option<&ItemRef> {
        self.recorded_helmet.as_ref()
    }

    /// Remember the normal helmet that was equipped before the head became armor.
    ///
    /// Only a genuine Helmet may be recorded; an armor value that was already in
    /// the head slot when the file was opened is never treated as a helmet.
    pub fn record_helmet(&mut self, item: ItemRef) -> Result<(), String> {
        if item.item_type != crate::types::ItemType::Helmet {
            return Err("只能记录真正的普通头盔，不能把身体护甲当作普通头盔".into());
        }
        self.recorded_helmet = Some(item);
        Ok(())
    }

    /// Set the head slot target. Always allowed; the body lock does not apply.
    pub fn set_head(&mut self, item: ItemRef, label: impl Into<String>) {
        let mut after = self.intent.clone();
        after.head = SlotIntent::Set { item };
        self.push(Command::SetIntent {
            label: label.into(),
            before: Box::new(self.intent.clone()),
            after: Box::new(after.clone()),
        });
        self.intent = after;
    }

    /// Set the body slot target. Fails while the body is locked.
    pub fn set_body(&mut self, item: ItemRef, label: impl Into<String>) -> Result<(), String> {
        if self.body_locked {
            return Err("身体已锁定，请先解除锁定再选择身体护甲".into());
        }
        let mut after = self.intent.clone();
        after.body = SlotIntent::Set { item };
        self.push(Command::SetIntent {
            label: label.into(),
            before: Box::new(self.intent.clone()),
            after: Box::new(after.clone()),
        });
        self.intent = after;
        Ok(())
    }

    /// Change the body lock. Unlocking alone never changes the body value.
    pub fn set_body_locked(&mut self, locked: bool) {
        if self.body_locked == locked {
            return;
        }

        let before_intent = self.intent.clone();
        let mut after_intent = before_intent.clone();
        if locked {
            // Re-locking drops a pending body change; the disk value stays authoritative.
            after_intent.body = SlotIntent::Keep;
        }
        self.push(Command::SetBodyLock {
            before: self.body_locked,
            after: locked,
            before_intent: Box::new(before_intent),
            after_intent: Box::new(after_intent.clone()),
        });
        self.body_locked = locked;
        self.intent = after_intent;
    }

    /// Restore one slot to the disk value, leaving the other untouched.
    pub fn revert_head(&mut self) {
        let mut after = self.intent.clone();
        after.head = SlotIntent::Keep;
        if after != self.intent {
            self.push(Command::SetIntent {
                label: "恢复磁盘头部".into(),
                before: Box::new(self.intent.clone()),
                after: Box::new(after.clone()),
            });
            self.intent = after;
        }
    }

    /// Restore the body slot to the disk value.
    pub fn revert_body(&mut self) {
        let mut after = self.intent.clone();
        after.body = SlotIntent::Keep;
        if after != self.intent {
            self.push(Command::SetIntent {
                label: "恢复磁盘身体".into(),
                before: Box::new(self.intent.clone()),
                after: Box::new(after.clone()),
            });
            self.intent = after;
        }
    }

    /// Apply a whole intent as one undoable command (used by preset application).
    pub fn apply_intent(&mut self, intent: LoadoutIntent, label: impl Into<String>) {
        let label = label.into();
        let mut after = intent;
        if self.body_locked {
            after.body = SlotIntent::Keep;
        }
        self.push(Command::SetIntent {
            label,
            before: Box::new(self.intent.clone()),
            after: Box::new(after.clone()),
        });
        self.intent = after;
    }

    /// Clear the draft back to the disk values.
    pub fn clear(&mut self) {
        self.apply_intent(LoadoutIntent::default(), "清空修改");
    }

    /// Undo the most recent command.
    pub fn undo(&mut self) -> bool {
        let Some(command) = self.undo_stack.pop() else {
            return false;
        };
        match &command {
            Command::SetIntent { before, .. } => self.intent = (**before).clone(),
            Command::SetBodyLock {
                before,
                before_intent,
                ..
            } => {
                self.body_locked = *before;
                self.intent = (**before_intent).clone();
            }
        }
        self.redo_stack.push(command);
        true
    }

    /// Redo the most recently undone command.
    pub fn redo(&mut self) -> bool {
        let Some(command) = self.redo_stack.pop() else {
            return false;
        };
        match &command {
            Command::SetIntent { after, .. } => self.intent = (**after).clone(),
            Command::SetBodyLock {
                after,
                after_intent,
                ..
            } => {
                self.body_locked = *after;
                self.intent = (**after_intent).clone();
            }
        }
        self.undo_stack.push(command);
        true
    }

    /// Whether undo is available.
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Whether redo is available.
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Label of the next undo step.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo_stack.last().map(Command::label)
    }

    /// Record that the disk changed underneath this draft.
    ///
    /// The base snapshot and the user's intent are both preserved: resolution is
    /// always an explicit user choice.
    pub fn mark_conflict(&mut self, disk: &Snapshot, detected_at: impl Into<String>) {
        self.conflict = Some(ConflictInfo {
            base_revision: self.base.revision(),
            disk_revision: disk.revision(),
            detected_at: detected_at.into(),
            disk_head_id: disk.head_id(),
            disk_body_id: disk.body_id(),
        });
    }

    /// Whether this draft is in conflict with the disk.
    pub fn in_conflict(&self) -> bool {
        self.conflict.is_some()
    }

    /// Rebase onto a newer disk snapshot, keeping the current intent.
    ///
    /// This is the "re-apply my intent to the latest disk state" resolution: it
    /// clears the conflict, adopts the new baseline, and keeps the user's choices.
    pub fn rebase_onto(&mut self, disk: Snapshot, label: impl Into<String>) {
        self.base = disk;
        self.conflict = None;
        let label = label.into();
        self.push(Command::SetIntent {
            label,
            before: Box::new(self.intent.clone()),
            after: Box::new(self.intent.clone()),
        });
    }

    /// Discard the draft and follow the disk snapshot.
    pub fn abandon_to(&mut self, disk: Snapshot) {
        self.base = disk;
        self.conflict = None;
        self.intent = LoadoutIntent::default();
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// Update the baseline after a successful commit, clearing dirty state.
    pub fn accept_committed(&mut self, committed: Snapshot) {
        self.base = committed;
        self.conflict = None;
        self.intent = LoadoutIntent::default();
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    fn push(&mut self, command: Command) {
        self.undo_stack.push(command);
        self.redo_stack.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ItemRef, ItemType};

    fn snapshot(head: u32, body: u32, cape: u32) -> Snapshot {
        let mut payload = vec![0u8; 572_088];
        payload[OFFSET_HEAD..OFFSET_HEAD + 4].copy_from_slice(&head.to_le_bytes());
        payload[OFFSET_BODY..OFFSET_BODY + 4].copy_from_slice(&body.to_le_bytes());
        payload[OFFSET_CAPE..OFFSET_CAPE + 4].copy_from_slice(&cape.to_le_bytes());
        Snapshot {
            raw: vec![0u8; 64],
            payload,
            sha256: "a".repeat(64),
            captured_at: "2026-09-19T00:00:00Z".into(),
            path: "test.sav".into(),
            writable: true,
            readonly_reason: None,
            outer_crc32: 0,
            inner_low32: 0,
        }
    }

    fn item(id: u32, item_type: ItemType, name: &str) -> ItemRef {
        ItemRef {
            item_key: format!("{}:0x{id:08X}", item_type.key_prefix()),
            id_u32: id,
            item_type,
            label_snapshot: name.into(),
        }
    }

    #[test]
    fn keep_produces_no_target_id() {
        let draft = Draft::new(snapshot(1, 2, 3));
        assert_eq!(draft.intent().head.target_id(), None);
        assert_eq!(draft.intent().body.target_id(), None);
        assert!(!draft.is_dirty());
    }

    #[test]
    fn body_is_unlocked_by_default_but_can_still_be_protected() {
        let mut draft = Draft::new(snapshot(1, 2, 3));
        assert!(!draft.body_locked());
        draft
            .set_body(item(9, ItemType::Armor, "测试护甲"), "设置身体")
            .unwrap();
        assert_eq!(draft.intent().body.target_id(), Some(9));

        draft.set_body_locked(true);
        let result = draft.set_body(item(10, ItemType::Armor, "另一件护甲"), "设置身体");
        assert!(
            result.is_err(),
            "explicitly locked body must refuse the edit"
        );
        assert_eq!(draft.intent().body.target_id(), None);
    }

    #[test]
    fn head_accepts_armor_and_leaves_body_alone() {
        let mut draft = Draft::new(snapshot(1, 2, 3));
        draft.set_head(
            item(0xD346_1392, ItemType::Armor, "FS-55 身体护甲"),
            "设置头部",
        );
        assert_eq!(draft.intent().head.target_id(), Some(0xD346_1392));
        assert_eq!(draft.intent().body.target_id(), None);
        assert!(draft.is_dirty());
    }

    #[test]
    fn unlock_then_set_body_then_undo_restores_both() {
        let mut draft = Draft::new(snapshot(1, 2, 3));
        draft.set_head(
            item(0x0568_48E9, ItemType::Helmet, "FS-55 头盔"),
            "设置头部",
        );
        draft.set_body_locked(false);
        draft
            .set_body(item(0x61B3_1723, ItemType::Armor, "B-01 护甲"), "设置身体")
            .unwrap();
        assert_eq!(draft.intent().head.target_id(), Some(0x0568_48E9));
        assert_eq!(draft.intent().body.target_id(), Some(0x61B3_1723));

        // Undo the body set.
        assert!(draft.undo());
        assert_eq!(draft.intent().body.target_id(), None);
        assert_eq!(draft.intent().head.target_id(), Some(0x0568_48E9));
    }

    #[test]
    fn applying_a_preset_pair_is_one_undo_step() {
        let mut draft = Draft::new(snapshot(1, 2, 3));
        let intent = LoadoutIntent {
            head: SlotIntent::Set {
                item: item(0xD346_1392, ItemType::Armor, "FS-55 护甲"),
            },
            body: SlotIntent::Set {
                item: item(0x61B3_1723, ItemType::Armor, "B-01 护甲"),
            },
        };
        draft.set_body_locked(false);
        draft.apply_intent(intent, "应用预设 双甲");
        assert_eq!(draft.intent().head.target_id(), Some(0xD346_1392));
        assert_eq!(draft.intent().body.target_id(), Some(0x61B3_1723));

        assert!(draft.undo(), "one undo must revert the whole pair");
        assert_eq!(draft.intent().head.target_id(), None);
        assert_eq!(draft.intent().body.target_id(), None);
    }

    #[test]
    fn set_to_current_value_is_still_a_dirty_intent_but_produces_no_patch_later() {
        // Domain-level: the intent records the choice; validation decides patches.
        let mut draft = Draft::new(snapshot(0x0568_48E9, 2, 3));
        draft.set_head(
            item(0x0568_48E9, ItemType::Helmet, "FS-55 头盔"),
            "设置头部",
        );
        assert_eq!(draft.intent().head.target_id(), Some(0x0568_48E9));
    }

    #[test]
    fn armor_already_in_head_is_never_recorded_as_a_helmet() {
        let mut draft = Draft::new(snapshot(0xD346_1392, 2, 3));
        let result = draft.record_helmet(item(0xD346_1392, ItemType::Armor, "FS-55 护甲"));
        assert!(result.is_err());
        assert!(draft.recorded_helmet().is_none());

        draft
            .record_helmet(item(0x261C_4A52, ItemType::Helmet, "B-01 头盔"))
            .unwrap();
        assert_eq!(
            draft.recorded_helmet().map(|item| item.id_u32),
            Some(0x261C_4A52)
        );
    }

    #[test]
    fn conflict_preserves_intent_and_rebase_keeps_choices() {
        let mut draft = Draft::new(snapshot(1, 2, 3));
        draft.set_head(item(0xD346_1392, ItemType::Armor, "FS-55 护甲"), "设置头部");
        let newer = snapshot(0x9999, 0x8888, 3);
        draft.mark_conflict(&newer, "2026-09-19T01:00:00Z");
        assert!(draft.in_conflict());
        assert_eq!(draft.intent().head.target_id(), Some(0xD346_1392));

        draft.rebase_onto(newer.clone(), "以最新磁盘重新应用意图");
        assert!(!draft.in_conflict());
        assert_eq!(draft.intent().head.target_id(), Some(0xD346_1392));
        assert_eq!(draft.base().head_id(), Some(0x9999));
    }

    #[test]
    fn relocking_body_drops_pending_body_change() {
        let mut draft = Draft::new(snapshot(1, 2, 3));
        draft.set_body_locked(false);
        draft
            .set_body(item(0x61B3_1723, ItemType::Armor, "B-01 护甲"), "设置身体")
            .unwrap();
        assert!(draft.intent().body.is_set());
        draft.set_body_locked(true);
        assert_eq!(draft.intent().body.target_id(), None);
    }

    #[test]
    fn relocking_body_is_one_atomic_undo_step() {
        let mut draft = Draft::new(snapshot(1, 2, 3));
        draft.set_body_locked(false);
        draft
            .set_body(item(0x61B3_1723, ItemType::Armor, "B-01 护甲"), "设置身体")
            .unwrap();

        draft.set_body_locked(true);
        assert!(draft.body_locked());
        assert_eq!(draft.intent().body.target_id(), None);

        assert!(draft.undo());
        assert!(!draft.body_locked(), "undo must restore the unlocked state");
        assert_eq!(
            draft.intent().body.target_id(),
            Some(0x61B3_1723),
            "undo must restore the body choice in the same step"
        );

        assert!(draft.redo());
        assert!(draft.body_locked());
        assert_eq!(draft.intent().body.target_id(), None);
    }

    #[test]
    fn undo_redo_round_trips() {
        let mut draft = Draft::new(snapshot(1, 2, 3));
        draft.set_head(
            item(0x0568_48E9, ItemType::Helmet, "FS-55 头盔"),
            "设置头部",
        );
        draft.undo();
        assert_eq!(draft.intent().head.target_id(), None);
        assert!(draft.can_redo());
        draft.redo();
        assert_eq!(draft.intent().head.target_id(), Some(0x0568_48E9));
    }
}
