use crate::rng::Gen;
use alloy_primitives::{Address, Bytes};
use evm2::SpecId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PrecompileTarget {
    number: u16,
    since: SpecId,
}

impl PrecompileTarget {
    pub(crate) fn address(self) -> Address {
        let mut bytes = [0; 20];
        bytes[18..].copy_from_slice(&self.number.to_be_bytes());
        Address::new(bytes)
    }

    pub(crate) const fn feature(self) -> &'static str {
        match self.number {
            0x01 => "precompile_ecrecover",
            0x02 => "precompile_sha256",
            0x03 => "precompile_ripemd160",
            0x04 => "precompile_identity",
            0x05 => "precompile_modexp",
            0x06 => "precompile_bn254_add",
            0x07 => "precompile_bn254_mul",
            0x08 => "precompile_bn254_pairing",
            0x09 => "precompile_blake2f",
            0x0a => "precompile_kzg_point_evaluation",
            0x0b => "precompile_bls12_g1_add",
            0x0c => "precompile_bls12_g1_msm",
            0x0d => "precompile_bls12_g2_add",
            0x0e => "precompile_bls12_g2_msm",
            0x0f => "precompile_bls12_pairing",
            0x10 => "precompile_bls12_map_fp_to_g1",
            0x11 => "precompile_bls12_map_fp2_to_g2",
            0x100 => "precompile_p256verify",
            _ => "precompile_unknown",
        }
    }

    pub(crate) const fn is_enabled(self, spec: SpecId) -> bool {
        spec.enables(self.since)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrecompileInput {
    pub(crate) bytes: Bytes,
    pub(crate) shape: &'static str,
}

const PRECOMPILES: &[PrecompileTarget] = &[
    PrecompileTarget { number: 0x01, since: SpecId::FRONTIER },
    PrecompileTarget { number: 0x02, since: SpecId::FRONTIER },
    PrecompileTarget { number: 0x03, since: SpecId::FRONTIER },
    PrecompileTarget { number: 0x04, since: SpecId::FRONTIER },
    PrecompileTarget { number: 0x05, since: SpecId::BYZANTIUM },
    PrecompileTarget { number: 0x06, since: SpecId::BYZANTIUM },
    PrecompileTarget { number: 0x07, since: SpecId::BYZANTIUM },
    PrecompileTarget { number: 0x08, since: SpecId::BYZANTIUM },
    PrecompileTarget { number: 0x09, since: SpecId::ISTANBUL },
    PrecompileTarget { number: 0x0a, since: SpecId::CANCUN },
    PrecompileTarget { number: 0x0b, since: SpecId::PRAGUE },
    PrecompileTarget { number: 0x0c, since: SpecId::PRAGUE },
    PrecompileTarget { number: 0x0d, since: SpecId::PRAGUE },
    PrecompileTarget { number: 0x0e, since: SpecId::PRAGUE },
    PrecompileTarget { number: 0x0f, since: SpecId::PRAGUE },
    PrecompileTarget { number: 0x10, since: SpecId::PRAGUE },
    PrecompileTarget { number: 0x11, since: SpecId::PRAGUE },
    PrecompileTarget { number: 0x100, since: SpecId::OSAKA },
];

pub(crate) const fn targets() -> &'static [PrecompileTarget] {
    PRECOMPILES
}

pub(crate) fn target_for_address(address: Address) -> Option<PrecompileTarget> {
    let bytes = address.as_slice();
    if bytes[..18].iter().any(|byte| *byte != 0) {
        return None;
    }
    let number = u16::from_be_bytes([bytes[18], bytes[19]]);
    PRECOMPILES.iter().copied().find(|target| target.number == number)
}

pub(crate) fn random_target(rng: &mut Gen, spec: SpecId) -> PrecompileTarget {
    let include_future = rng.one_in(20);
    let mut candidates = Vec::new();
    for target in PRECOMPILES {
        if target.is_enabled(spec) || include_future {
            candidates.push(*target);
        }
    }
    rng.pick(&candidates)
}

pub(crate) fn input(rng: &mut Gen, target: PrecompileTarget) -> PrecompileInput {
    let bytes = match rng.range(5) {
        0 => Bytes::new(),
        1 => exact_input(rng, target).into(),
        2 => {
            let len = exact_len(rng, target).saturating_sub(1);
            rng.bytes(len).into()
        }
        3 => {
            let len = exact_len(rng, target).saturating_add(rng.pick(&[1, 32, 96]));
            rng.bytes(len).into()
        }
        _ => {
            let len = rng.pick(&[1, 4, 20, 31, 32, 64, 96, 128, 192, 213, 256]);
            rng.bytes(len).into()
        }
    };
    let shape = input_shape(target, bytes.len());
    PrecompileInput { bytes, shape }
}

pub(crate) fn input_shape(target: PrecompileTarget, len: usize) -> &'static str {
    if len == 0 {
        return "empty";
    }
    let exact = exact_lens(target);
    if exact.contains(&len) {
        return "exact";
    }
    if exact.iter().any(|exact| len < *exact) {
        return "short";
    }
    if exact.iter().any(|exact| len > *exact) {
        return "long";
    }
    "arbitrary"
}

fn exact_len(rng: &mut Gen, target: PrecompileTarget) -> usize {
    rng.pick(exact_lens(target))
}

const fn exact_lens(target: PrecompileTarget) -> &'static [usize] {
    match target.number {
        0x01 => &[128],
        0x02..=0x04 => &[32, 64, 128],
        0x05 => &[96, 99],
        0x06 => &[128],
        0x07 => &[96],
        0x08 => &[192, 384],
        0x09 => &[213],
        0x0a => &[192],
        0x0b => &[256],
        0x0c => &[160, 320],
        0x0d => &[512],
        0x0e => &[288, 576],
        0x0f => &[384, 768],
        0x10 => &[64],
        0x11 => &[128],
        0x100 => &[160],
        _ => &[32],
    }
}

fn exact_input(rng: &mut Gen, target: PrecompileTarget) -> Vec<u8> {
    match target.number {
        0x01 => ecrecover_input(rng),
        0x05 => modexp_input(rng),
        0x09 => blake2f_input(rng),
        number @ (0x06..=0x08 | 0x0a..=0x11 | 0x100) => {
            let len = exact_len(rng, target);
            if rng.one_in(2) {
                vec![0; len]
            } else {
                let mut bytes = rng.bytes(len);
                if number == 0x100 && bytes.len() == 160 {
                    bytes[32] = 0;
                }
                bytes
            }
        }
        _ => {
            let len = exact_len(rng, target);
            rng.bytes(len)
        }
    }
}

fn ecrecover_input(rng: &mut Gen) -> Vec<u8> {
    let mut input = if rng.one_in(2) { vec![0; 128] } else { rng.bytes(128) };
    input[63] = rng.pick(&[0, 1, 27, 28, 35]);
    input
}

fn modexp_input(rng: &mut Gen) -> Vec<u8> {
    match rng.range(3) {
        0 => vec![0; 96],
        1 => {
            let mut input = vec![0; 99];
            input[31] = 1;
            input[63] = 1;
            input[95] = 1;
            input[96] = 2;
            input[97] = 3;
            input[98] = 5;
            input
        }
        _ => {
            let mut input = vec![0; 96];
            input[30] = 4;
            input[62] = 4;
            input[94] = 4;
            input
        }
    }
}

fn blake2f_input(rng: &mut Gen) -> Vec<u8> {
    let mut input = rng.bytes(213);
    input[..4].copy_from_slice(&rng.pick(&[0_u32, 1, 12, 16]).to_be_bytes());
    input[212] = rng.pick(&[0, 1, 2]);
    input
}
