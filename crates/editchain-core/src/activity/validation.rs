//! Structural validation does not invent missing observations.

use std::fmt;

use super::{ContentUpdate, FileAction, Kind, Operation, Stage, TurnAction};
use crate::FileEdit;

/// Invalid schema-three fields; storage refuses the entire record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationError(pub &'static str);

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ValidationError {}

fn require(valid: bool, message: &'static str) -> Result<(), ValidationError> {
    if valid {
        Ok(())
    } else {
        Err(ValidationError(message))
    }
}

impl Operation {
    /// Validate one immutable record, without requiring all referenced events.
    /// # Errors
    /// Returns the first inconsistent field contract.
    pub fn validate(&self) -> Result<(), ValidationError> {
        require(
            !self.parents.contains(&self.id),
            "operation cannot parent itself",
        )?;
        let unique: std::collections::BTreeSet<_> = self.parents.iter().collect();
        require(
            unique.len() == self.parents.len(),
            "duplicate causal parent",
        )?;
        require(
            self.original.as_ref().is_none_or(|original| {
                original.operation != self.id && !original.converter.is_empty()
            }),
            "invalid original reference",
        )?;
        if let Some(legacy) = &self.legacy {
            require(
                legacy.operation != self.id,
                "converted bytes require a new operation ID",
            )?;
            require(
                legacy
                    .source
                    .is_none_or(|source| source.id() == legacy.operation),
                "legacy tuple does not match the old ID",
            )?;
        }
        match &self.kind {
            Kind::Turn(turn) => {
                require(self.session.is_some(), "turn requires a session")?;
                require(
                    self.turn == Some(self.item),
                    "turn envelope must identify the same logical turn",
                )?;
                require(
                    (turn.action == TurnAction::Finished) == turn.outcome.is_some(),
                    "only finished turns carry completion outcomes",
                )
            }
            Kind::Message(message) => {
                require(
                    message.outcome.is_none() || message.stage == Stage::Finished,
                    "message outcome requires finished stage",
                )?;
                let mut blocks = std::collections::BTreeSet::new();
                for block in &message.blocks {
                    require(
                        blocks.insert(block.block),
                        "duplicate message block in one event",
                    )?;
                    self.validate_block(block)?;
                }
                Ok(())
            }
            Kind::Tool(tool) => {
                require(
                    (tool.stage == Stage::Finished) == tool.outcome.is_some(),
                    "finished tool requires an explicit outcome",
                )?;
                require(
                    tool.parent_call != Some(self.item),
                    "tool cannot be its own parent call",
                )?;
                if let Some(block) = &tool.output {
                    self.validate_block(block)?;
                }
                require(
                    tool.terminal.as_ref().is_none_or(|terminal| {
                        terminal.exit_code.is_none() || tool.stage == Stage::Finished
                    }),
                    "exit code requires a finished tool",
                )
            }
            Kind::File(file) => {
                require(
                    file.ranges.iter().all(|range| range.start <= range.end),
                    "reversed byte range",
                )?;
                require(
                    file.text_ranges
                        .iter()
                        .all(|range| range.start <= range.end),
                    "reversed UTF-16 range",
                )?;
                if let FileEdit::ReplaceBytes { range, .. } = &file.edit {
                    require(range.start <= range.end, "reversed replacement range")?;
                }
                require(
                    file.action != FileAction::Rename || file.renamed_to.is_some(),
                    "rename requires a destination",
                )?;
                require(
                    file.change.is_none()
                        || matches!(
                            file.action,
                            FileAction::Create
                                | FileAction::Change
                                | FileAction::Rename
                                | FileAction::Delete
                        ),
                    "proposed/applied state belongs to a file modification",
                )
            }
            Kind::Note(note) => require(note.version > 0, "note content version must be positive"),
            Kind::Original(original) => require(
                !original.provider.is_empty(),
                "original provider must be explicit or unknown",
            ),
            Kind::Link(link) => require(!link.relation.is_empty(), "link relation cannot be empty"),
            Kind::Session(_) | Kind::Commit(_) | Kind::Author(_) => Ok(()),
        }
    }

    fn validate_block(&self, block: &ContentUpdate) -> Result<(), ValidationError> {
        require(
            block.previous != Some(self.id),
            "content cannot reference itself as its predecessor",
        )
    }
}
