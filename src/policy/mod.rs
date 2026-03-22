use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::policy::id::PolicyId;

pub trait Policy: Send + Sync {
    fn id(&self) -> PolicyId {
        PolicyId::from_str_lossy(self.name())
    }

    fn name(&self) -> &str;

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult;
}

pub mod clang_format;
pub mod clang_format_service;
pub mod class_layout;
pub mod compact_decls;
pub mod dash_comment;
pub mod void_params;
pub mod include_guards;
pub mod include_order;
pub mod keyword_operators;
pub mod macro_spacing;
pub mod ns_comments;
pub mod naming_conventions;
pub mod numeric_suffix;
pub mod op_spacing;
pub mod factory;
pub mod id;
pub mod registry;
pub mod pragma_once;
pub mod section_title;
pub mod snake_case;
pub mod stub;
pub mod text_utils;
pub mod zone;
