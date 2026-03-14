use std::collections::HashMap;

use anyhow::{anyhow, Result};
use toml::Table;

use crate::config::enums::Enforcement;
use crate::config::enums::PolicyType;
use crate::config::enums::TouchContract;

#[derive(Clone, Debug)]
pub struct PolicyConfig {
    pub name: String,
    pub enabled: bool,
    pub policy_type: PolicyType,
    pub touch_contract: TouchContract,
    pub enforcement: Enforcement,
    pub raw: Table,
}

impl PolicyConfig {
    pub fn from_policy_table(table: &Table) -> Result<Self> {
        let name = table
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("policy table missing required 'name'"))?
            .to_string();

        let enabled = table
            .get("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let policy_type = table
            .get("type")
            .and_then(|value| value.as_str())
            .map(PolicyType::from_value)
            .unwrap_or(PolicyType::Python);
        let touch_contract = table
            .get("touch_contract")
            .and_then(|value| value.as_str())
            .map(TouchContract::from_value)
            .unwrap_or(TouchContract::Any);
        let enforcement = table
            .get("enforcement")
            .and_then(|value| value.as_str())
            .map(Enforcement::from_value)
            .unwrap_or(Enforcement::Hard);

        Ok(Self {
            name,
            enabled,
            policy_type,
            touch_contract,
            enforcement,
            raw: table.clone(),
        })
    }

    pub fn merge(&mut self, table: &Table) {
        for (key, value) in table {
            self.raw.insert(key.clone(), value.clone());
        }
        self.enabled = self
            .raw
            .get("enabled")
            .and_then(|value| value.as_bool())
            .unwrap_or(self.enabled);
        self.policy_type = self
            .raw
            .get("type")
            .and_then(|value| value.as_str())
            .map(PolicyType::from_value)
            .unwrap_or_else(|| self.policy_type.clone());
        self.touch_contract = self
            .raw
            .get("touch_contract")
            .and_then(|value| value.as_str())
            .map(TouchContract::from_value)
            .unwrap_or_else(|| self.touch_contract.clone());
        self.enforcement = self
            .raw
            .get("enforcement")
            .and_then(|value| value.as_str())
            .map(Enforcement::from_value)
            .unwrap_or(self.enforcement);
    }

    pub fn string_value(&self, key: &str) -> Option<String> {
        self.raw
            .get(key)
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
    }

    pub fn bool_value(&self, key: &str) -> Option<bool> {
        self.raw.get(key).and_then(|value| value.as_bool())
    }

    pub fn int_value(&self, key: &str) -> Option<i64> {
        self.raw.get(key).and_then(|value| value.as_integer())
    }

    pub fn usize_value(&self, key: &str) -> Option<usize> {
        self.int_value(key)
            .and_then(|value| (value >= 0).then_some(value as usize))
    }

    pub fn string_list_value(&self, key: &str) -> Vec<String> {
        self.raw
            .get(key)
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|value| value.as_str())
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn table_value(&self, key: &str) -> Option<&Table> {
        self.raw.get(key).and_then(|value| value.as_table())
    }

    pub fn table_string_map(&self, key: &str) -> HashMap<String, String> {
        let mut result = HashMap::new();
        let Some(table) = self.table_value(key) else {
            return result;
        };
        for (map_key, map_value) in table {
            if let Some(value) = map_value.as_str() {
                result.insert(map_key.to_lowercase(), value.to_string());
            }
        }
        result
    }

    pub fn has_key(&self, key: &str) -> bool {
        self.raw.contains_key(key)
    }

    pub fn semantic_hard_invariant(&self) -> bool {
        self.bool_value("semantic_hard_invariant").unwrap_or(false)
            || self.enforcement == Enforcement::Must
    }
}
