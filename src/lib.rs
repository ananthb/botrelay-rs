//! # botrelay
//!
//! Reusable primitives for Cloudflare Workers that forward content to
//! Telegram/Discord bots and route replies back.
//!
//! See [`telegram`], [`discord`], and [`reply`].

pub mod discord;
pub mod reply;
pub mod telegram;

pub use reply::ReplyContext;
