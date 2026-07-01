#[path = "fuzz_targets/precompile_case.rs"]
#[allow(dead_code)]
mod precompile_case;

use arbitrary::Dearbitrary;
use precompile_case::{PrecompileAddress, PrecompileCase};
use std::{fs, io, path::Path};

const SPECS: &[&str] = &[
    "frontier",
    "homestead",
    "tangerine",
    "spurious_dragon",
    "byzantium",
    "petersburg",
    "istanbul",
    "berlin",
    "london",
    "merge",
    "shanghai",
    "cancun",
    "prague",
    "osaka",
    "amsterdam",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=fuzz_targets/precompile_case.rs");
    write_precompile_seeds().expect("failed to write precompile seed corpus");
}

fn write_precompile_seeds() -> io::Result<()> {
    for spec in SPECS {
        let target = format!("precompile_compare_{spec}");
        let dir = Path::new("corpus").join(target);
        fs::create_dir_all(&dir)?;
        for (name, case) in precompile_seeds() {
            write_seed(&dir.join(name), &case.to_arbitrary_bytes())?;
        }
    }
    Ok(())
}

fn write_seed(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    fs::write(path, bytes)
}

fn precompile_seeds() -> Vec<(&'static str, PrecompileCase)> {
    vec![
        seed("ec_recover_empty", PrecompileAddress::EcRecover, 3_000, 32, 128),
        seed("sha256_word", PrecompileAddress::Sha256, 3_000, 32, 32),
        seed("ripemd160_word", PrecompileAddress::Ripemd160, 3_000, 32, 32),
        seed("identity_word", PrecompileAddress::Identity, 3_000, 32, 32),
        seed("modexp_header", PrecompileAddress::ModExp, 200_000, 32, 96),
        seed("bn254_add_zero", PrecompileAddress::Bn254Add, 150_000, 64, 128),
        seed("bn254_mul_zero", PrecompileAddress::Bn254Mul, 150_000, 64, 96),
        seed("bn254_pairing_zero", PrecompileAddress::Bn254Pairing, 500_000, 32, 192),
        seed("blake2f_empty", PrecompileAddress::Blake2F, 500_000, 64, 213),
        seed("kzg_point_eval_zero", PrecompileAddress::KzgPointEvaluation, 500_000, 64, 192),
        seed("bls_g1_add_infinity", PrecompileAddress::BlsG1Add, 500_000, 128, 256),
        seed("bls_g1_msm_infinity", PrecompileAddress::BlsG1Msm, 500_000, 128, 160),
        seed("bls_g2_add_infinity", PrecompileAddress::BlsG2Add, 500_000, 256, 512),
        seed("bls_g2_msm_infinity", PrecompileAddress::BlsG2Msm, 500_000, 256, 288),
        seed("bls_pairing_infinity", PrecompileAddress::BlsPairing, 500_000, 32, 384),
        seed("bls_map_fp_to_g1_zero", PrecompileAddress::BlsMapFpToG1, 500_000, 128, 64),
        seed("bls_map_fp2_to_g2_zero", PrecompileAddress::BlsMapFp2ToG2, 500_000, 256, 128),
        seed("p256_verify_zero", PrecompileAddress::P256Verify, 500_000, 32, 160),
    ]
}

fn seed(
    name: &'static str,
    address: PrecompileAddress,
    gas: u64,
    return_len: u16,
    input_len: usize,
) -> (&'static str, PrecompileCase) {
    (name, PrecompileCase { address, gas, return_len, is_static: true, input: vec![0; input_len] })
}
