#![feature(async_closure)]
#![feature(try_trait_v2)]
#![feature(type_alias_impl_trait, impl_trait_in_assoc_type)]
#![feature(never_type)]
#![feature(backtrace_frames)]
#![feature(error_generic_member_access)]
#![feature(type_name_of_val)]
#![feature(provide_any)]
#![feature(let_chains)]
#![feature(closure_lifetime_binder)]
#![deny(unused_must_use)]

pub mod api;
pub mod builder;
mod error;
mod executor;
mod graph;
mod package;
mod rule;
mod runtime;
mod vfs;
mod workspace;

#[cfg(test)]
mod test_util;
