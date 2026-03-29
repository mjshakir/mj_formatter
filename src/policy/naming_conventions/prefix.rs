use super::{IdentifierContext, NamingConventionsPolicy};

#[derive(Clone, Debug)]
pub struct PrefixConfig {
    pub(crate) local: Box<str>,
    pub(crate) member: Box<str>,
    pub(crate) global: Box<str>,
    pub(crate) static_lower: Box<str>,
    pub(crate) static_upper: Box<str>,
    pub(crate) const_lower: Box<str>,
    pub(crate) constexpr_upper: Box<str>,
    pub(crate) volatile: Box<str>,
    pub(crate) pointer: Box<str>,
    pub(crate) shared_ptr: Box<str>,
    pub(crate) unique_ptr: Box<str>,
    pub(crate) weak_ptr: Box<str>,
    pub(crate) function: Box<str>,
    pub(crate) reference: Box<str>,
    pub(crate) atomic: Box<str>,
    pub(crate) enum_var: Box<str>,
    pub(crate) struct_var: Box<str>,
}

impl Default for PrefixConfig {
    fn default() -> Self {
        Self {
            local: "_".into(),
            member: "m_".into(),
            global: "g_".into(),
            static_lower: "s_".into(),
            static_upper: "S_".into(),
            const_lower: "c_".into(),
            constexpr_upper: "C_".into(),
            volatile: "v_".into(),
            pointer: "p_".into(),
            shared_ptr: "sp_".into(),
            unique_ptr: "up_".into(),
            weak_ptr: "wp_".into(),
            function: "f_".into(),
            reference: "r_".into(),
            atomic: "a_".into(),
            enum_var: "e_".into(),
            struct_var: "t_".into(),
        }
    }
}

impl PrefixConfig {
    pub(super) fn has_known_prefix(&self, name: &str) -> bool {
        let candidates = [
            &*self.local, &*self.member, &*self.global,
            &*self.static_lower, &*self.static_upper,
            &*self.const_lower, &*self.constexpr_upper,
            &*self.volatile, &*self.pointer, &*self.shared_ptr,
            &*self.unique_ptr, &*self.weak_ptr, &*self.function,
            &*self.reference, &*self.atomic, &*self.enum_var,
            &*self.struct_var,
        ];
        candidates
            .iter()
            .any(|pfx| !pfx.is_empty() && name.starts_with(pfx))
    }
}

impl NamingConventionsPolicy {
    #[cfg(test)]
    pub(super) fn build_stacked_prefix(&self, ctx: &IdentifierContext<'_>) -> String {
        let mut buf = String::with_capacity(16);
        self.build_stacked_prefix_into(&mut buf, ctx);
        buf
    }

    pub(super) fn build_stacked_prefix_into(&self, prefix: &mut String, ctx: &IdentifierContext<'_>) {
        prefix.clear();
        let candidates = &self.prefixes;

        if ctx.is_field {
            prefix.push_str(&candidates.member);
        } else if ctx.is_global {
            prefix.push_str(&candidates.global);
        } else {
            prefix.push_str(&candidates.local);
        }

        if ctx.ts_static {
            prefix.push_str(&candidates.static_lower);
        }
        if ctx.ts_const {
            prefix.push_str(&candidates.const_lower);
        }
        if ctx.ts_volatile {
            prefix.push_str(&candidates.volatile);
        }
        let tmpl = ctx.template_base_name;
        let type_pfx = if ctx.ts_pointer
            || ctx.canonical_type_kind == clang_sys::CXType_Pointer as i32
        {
            match tmpl {
                Some("shared_ptr") => &candidates.shared_ptr,
                Some("unique_ptr") => &candidates.unique_ptr,
                Some("weak_ptr") => &candidates.weak_ptr,
                _ => &candidates.pointer,
            }
        } else if ctx.ts_reference
            || ctx.canonical_type_kind == clang_sys::CXType_LValueReference as i32
            || ctx.canonical_type_kind == clang_sys::CXType_RValueReference as i32
        {
            &candidates.reference
        } else if ctx.num_template_args > 0 {
            match tmpl {
                Some("shared_ptr") => &candidates.shared_ptr,
                Some("unique_ptr") => &candidates.unique_ptr,
                Some("weak_ptr") => &candidates.weak_ptr,
                Some("function") | Some("Function") => &candidates.function,
                Some("atomic") | Some("Atomic") => &candidates.atomic,
                _ => "",
            }
        } else {
            ""
        };
        prefix.push_str(type_pfx);
    }
}
