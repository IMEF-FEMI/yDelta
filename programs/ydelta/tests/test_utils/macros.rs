//! Error-assertion macros for tests. Pattern lifted from numa's
//! `tests/test_utils/macros.rs` so failures pinpoint the exact
//! `YdeltaError` or `InstructionError` variant rather than a stringy
//! `msg.contains("Custom")` check.

#[macro_export]
macro_rules! assert_custom_error {
    ($result:expr, $matcher:expr) => {
        match $result {
            std::result::Result::Ok(_) => panic!("Expected error but got success"),
            std::result::Result::Err(solana_program_test::BanksClientError::TransactionError(
                solana_sdk::transaction::TransactionError::InstructionError(
                    _,
                    solana_program::instruction::InstructionError::Custom(n),
                ),
            ))
            | std::result::Result::Err(solana_program_test::BanksClientError::SimulationError {
                err:
                    solana_sdk::transaction::TransactionError::InstructionError(
                        _,
                        solana_program::instruction::InstructionError::Custom(n),
                    ),
                ..
            }) => {
                let expected = $matcher as u32;
                assert_eq!(
                    n, expected,
                    "Expected custom error {:?} ({}) but got Custom({})",
                    $matcher, expected, n
                );
            }
            std::result::Result::Err(other) => {
                panic!("expected custom error {:?}, got: {:?}", $matcher, other)
            }
        }
    };
}

#[macro_export]
macro_rules! assert_program_error {
    ($result:expr, $matcher:expr) => {
        match $result {
            std::result::Result::Ok(_) => panic!("Expected error but got success"),
            std::result::Result::Err(solana_program_test::BanksClientError::TransactionError(
                solana_sdk::transaction::TransactionError::InstructionError(_, ie),
            ))
            | std::result::Result::Err(solana_program_test::BanksClientError::SimulationError {
                err: solana_sdk::transaction::TransactionError::InstructionError(_, ie),
                ..
            }) => {
                assert_eq!(
                    ie, $matcher,
                    "Expected instruction error {:?} but got {:?}",
                    $matcher, ie
                );
            }
            std::result::Result::Err(other) => {
                panic!(
                    "expected instruction error {:?}, got: {:?}",
                    $matcher, other
                )
            }
        }
    };
}
