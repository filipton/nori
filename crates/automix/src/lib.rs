//! AutoMix over the app's database: the analysis store, the transition planner the engine asks and the
//! beat model's file. Analysis and planning themselves are `nori_player::automix`.

pub mod beat_model;
pub mod planner;
pub mod store;

pub use nori_player::automix::*;
