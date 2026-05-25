//! Program-side module roots and re-exports. Splits the program crate into
//! [`error`] (`YdeltaError`), [`instruction`] (`YdeltaInstruction` tag enum),
//! [`processor`] (one handler module per instruction), and
//! [`instruction_builders`] (client-side `Instruction` constructors).
//! The `pub use` re-exports flatten error / instruction / processor symbols
//! into [`crate::program`] for shorter import paths in `lib.rs`.

pub mod error;
pub mod instruction;
pub mod instruction_builders;
pub mod processor;

pub use error::*;
pub use instruction::*;
pub use processor::*;
