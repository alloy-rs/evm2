#[derive(arbitrary::Arbitrary, arbitrary::Dearbitrary, Clone, Debug)]
pub struct PrecompileCase {
    pub address: PrecompileAddress,
    pub gas: u64,
    pub return_len: u16,
    pub is_static: bool,
    pub input: Vec<u8>,
}

#[derive(arbitrary::Arbitrary, arbitrary::Dearbitrary, Clone, Copy, Debug)]
pub enum PrecompileAddress {
    EcRecover,
    Sha256,
    Ripemd160,
    Identity,
    ModExp,
    Bn254Add,
    Bn254Mul,
    Bn254Pairing,
    Blake2F,
    KzgPointEvaluation,
    BlsG1Add,
    BlsG1Msm,
    BlsG2Add,
    BlsG2Msm,
    BlsPairing,
    BlsMapFpToG1,
    BlsMapFp2ToG2,
    P256Verify,
}

impl PrecompileAddress {
    pub const ALL: [Self; 18] = [
        Self::EcRecover,
        Self::Sha256,
        Self::Ripemd160,
        Self::Identity,
        Self::ModExp,
        Self::Bn254Add,
        Self::Bn254Mul,
        Self::Bn254Pairing,
        Self::Blake2F,
        Self::KzgPointEvaluation,
        Self::BlsG1Add,
        Self::BlsG1Msm,
        Self::BlsG2Add,
        Self::BlsG2Msm,
        Self::BlsPairing,
        Self::BlsMapFpToG1,
        Self::BlsMapFp2ToG2,
        Self::P256Verify,
    ];

    pub const fn number(self) -> u64 {
        match self {
            Self::EcRecover => 0x01,
            Self::Sha256 => 0x02,
            Self::Ripemd160 => 0x03,
            Self::Identity => 0x04,
            Self::ModExp => 0x05,
            Self::Bn254Add => 0x06,
            Self::Bn254Mul => 0x07,
            Self::Bn254Pairing => 0x08,
            Self::Blake2F => 0x09,
            Self::KzgPointEvaluation => 0x0a,
            Self::BlsG1Add => 0x0b,
            Self::BlsG1Msm => 0x0c,
            Self::BlsG2Add => 0x0d,
            Self::BlsG2Msm => 0x0e,
            Self::BlsPairing => 0x0f,
            Self::BlsMapFpToG1 => 0x10,
            Self::BlsMapFp2ToG2 => 0x11,
            Self::P256Verify => 0x100,
        }
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL[index % Self::ALL.len()]
    }
}
