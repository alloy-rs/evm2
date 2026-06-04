pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("failed to read {path}")]
    ReadInput {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to decode JSON from {path}")]
    DecodeJson {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to execute EEST state test fixture")]
    StateTest {
        #[source]
        source: evm2_eest::StateTestError,
    },
    #[error("failed to execute EEST blockchain test fixture")]
    BlockchainTest {
        #[source]
        source: evm2_eest::BlockchainTestError,
    },
    #[error("capture failed")]
    Capture {
        #[source]
        source: crate::capture::CaptureError,
    },
    #[error("fuzzer failed: {0}")]
    Fuzzer(String),
    #[error("could not detect EEST fixture kind in {path}")]
    UnknownFixtureKind { path: std::path::PathBuf },
}
