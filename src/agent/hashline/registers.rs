//! Session-scoped register bank backing hashline `@name` paste/cut storage.
//!
//! The `edit` tool holds an `Arc<RegisterBank>` so registers persist across
//! calls: `CUT ... @name` captures lines into a register and a later
//! `PUT ... @name` pastes them back out. `None` names the anonymous register,
//! used by colonless gap `PUT`s and unnamed `CUT`s.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{anyhow, Result};

/// Named (and anonymous) line registers, keyed by register name.
#[derive(Default)]
pub(crate) struct RegisterBank {
    storage: Mutex<HashMap<Option<String>, Vec<String>>>,
}

impl RegisterBank {
    /// Overwrites the register `name` (or the anonymous register when `name` is
    /// `None`) with `lines`.
    pub(crate) fn store(&self, name: Option<&str>, lines: Vec<String>) -> Result<()> {
        let mut storage = self.storage.lock().map_err(|_| anyhow!("register bank lock poisoned"))?;
        storage.insert(name.map(str::to_owned), lines);
        Ok(())
    }

    /// Returns a clone of the lines in register `name`, or `None` when the
    /// register has never been stored. Registers are re-pastable, so copies
    /// are returned rather than moved out.
    pub(crate) fn load(&self, name: Option<&str>) -> Result<Option<Vec<String>>> {
        let storage = self.storage.lock().map_err(|_| anyhow!("register bank lock poisoned"))?;
        Ok(storage.get(&name.map(str::to_owned)).cloned())
    }

    /// Removes every register entry.
    pub(crate) fn clear(&self) -> Result<()> {
        let mut storage = self.storage.lock().map_err(|_| anyhow!("register bank lock poisoned"))?;
        storage.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_load_named_register_roundtrip() {
        let bank = RegisterBank::default();
        bank.store(Some("reg"), vec!["a".into(), "b".into()]).unwrap();
        assert_eq!(bank.load(Some("reg")).unwrap(), Some(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn load_missing_register_is_none() {
        let bank = RegisterBank::default();
        assert_eq!(bank.load(Some("missing")).unwrap(), None);
        assert_eq!(bank.load(None).unwrap(), None);
    }

    #[test]
    fn store_overwrites_previous_lines() {
        let bank = RegisterBank::default();
        bank.store(Some("reg"), vec!["old".into()]).unwrap();
        bank.store(Some("reg"), vec!["new1".into(), "new2".into()]).unwrap();
        assert_eq!(bank.load(Some("reg")).unwrap(), Some(vec!["new1".into(), "new2".into()]));
    }

    #[test]
    fn anonymous_and_named_registers_are_independent() {
        let bank = RegisterBank::default();
        bank.store(None, vec!["anon".into()]).unwrap();
        bank.store(Some("anon"), vec!["named".into()]).unwrap();
        assert_eq!(bank.load(None).unwrap(), Some(vec!["anon".into()]));
        assert_eq!(bank.load(Some("anon")).unwrap(), Some(vec!["named".into()]));
    }

    #[test]
    fn load_returns_clone_and_leaves_register_intact() {
        let bank = RegisterBank::default();
        bank.store(Some("reg"), vec!["x".into()]).unwrap();
        let first = bank.load(Some("reg")).unwrap().unwrap();
        let second = bank.load(Some("reg")).unwrap().unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn clear_empties_every_register() {
        let bank = RegisterBank::default();
        bank.store(None, vec!["a".into()]).unwrap();
        bank.store(Some("b"), vec!["c".into()]).unwrap();
        bank.clear().unwrap();
        assert_eq!(bank.load(None).unwrap(), None);
        assert_eq!(bank.load(Some("b")).unwrap(), None);
    }
}