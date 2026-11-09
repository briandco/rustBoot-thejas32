mod curve;
mod fitsigner;
mod lmssigner;
mod mcusigner;

use curve::SigningKeyType;
use curve::{import_signing_key, CurveType};
use fitsigner::sign_fit;
use mcusigner::sign_mcu_image;
use rustBoot::dt::Reader;

use std::env;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};

fn main() {
    // let _ = log_init();

    let args = env::args().collect::<Vec<_>>();
    let args = args.iter().map(|s| &**s).collect::<Vec<_>>();

    // LMS commands (hybrid images). Handled first: the commands below expect
    // an ECDSA key file as the fourth argument.
    match args.get(1).copied() {
        Some("lms-keygen") => {
            let (Some(private), Some(public)) = (args.get(2), args.get(3)) else {
                eprintln!("usage: rbsigner lms-keygen <private-key-out> <public-key-out>");
                std::process::exit(2);
            };
            match lmssigner::keygen(std::path::Path::new(private), std::path::Path::new(public)) {
                Ok(()) => println!("LMS key pair written: {private} (secret, stateful: 1024 signatures) and {public}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("lms-sign") => {
            let (Some(image), Some(key)) = (args.get(2), args.get(3)) else {
                eprintln!("usage: rbsigner lms-sign <ecdsa-signed-image> <lms-private-key>");
                std::process::exit(2);
            };
            match lmssigner::append_trailer(std::path::Path::new(image), std::path::Path::new(key)) {
                Ok(n) => println!("LMS trailer appended: {image} is now {n} bytes (key {key} advanced)"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        _ => {}
    }

    let mut key_file = Vec::new();
    let mut kf = fs::File::open(args[4]).expect("Need path to key_file as argument");
    kf.read_to_end(&mut key_file).unwrap();
    let sk: SigningKeyType;

    match args[3] {
        "nistp256" => {
            let signing_key = &key_file.as_slice()[0x40..];
            if signing_key.len() != 32 {
                panic!("invalid nistp256 key: length is not 32 bytes")
            }
            sk = import_signing_key(CurveType::NistP256, signing_key).unwrap();
        }
        _ => {
            unimplemented!()
        }
    }

    let mut image_blob = Vec::new();
    match args[1] {
        "fit-image" => {
            let mut itb = fs::File::open(args[2]).expect("Need path to itb_blob as argument");
            itb.read_to_end(&mut image_blob).unwrap();

            // get the timestamp
            let reader = Reader::read(&image_blob.as_slice()).unwrap();
            let root = &reader.struct_items();
            let (_, node_iter) = root.path_struct_items("/").next().unwrap();

            let timestamp = match node_iter.get_node_property("timestamp") {
                Some(ts) => u32::from_be_bytes(ts.try_into().unwrap()),
                None => panic!("bad itb file, doesnt contain a timestamp"),
            };

            let version_string = timestamp.to_string();
            let version = timestamp;
            let output_itb_name = String::from(format!("signed-v{version_string}.itb").as_str());

            println!("\nImage type:       fit-image");
            println!("Curve type:       {}", args[3]);
            #[rustfmt::skip]
            println!("Input image:      {}.itb", String::from(args[2].rsplit_terminator(&['/', '.'][..]).collect::<Vec<_>>()[1]));
            println!("fit version:      {:?}", version);
            #[rustfmt::skip]
            println!("Public key:       {}.der", String::from(args[4].rsplit_terminator(&['/', '.'][..]).collect::<Vec<_>>()[1]));
            println!("Output image:     {}", output_itb_name);

            let signed_fit = sign_fit(image_blob, version, sk);
            match signed_fit {
                Ok(val) => {
                    // println!(
                    //     "signed_fit: {:?}",
                    //     &val.as_slice()[(val.len() - 1071)..(val.len() - 800)]
                    // );
                    let out_file = args[2].rsplit_once('/');
                    match out_file {
                        Some((f, _)) => {
                            let file = File::create(format!("{f}/{output_itb_name}").as_str());
                            match file {
                                Ok(mut file) => {
                                    let bytes_written = file.write(val.as_slice());
                                    if let Ok(val) = bytes_written {
                                        println!("\nbytes_written: {:?}", val);
                                    }
                                }
                                Err(e) => panic!("error: {:?}", e),
                            }
                        }
                        None => {
                            panic!("something's wrong with your file_path to itb_blob ")
                        }
                    }
                }
                Err(_e) => {}
            }
        }
        "mcu-image" => {
            //String concatenation
            let image_version_args = String::from(args[5]);
            #[rustfmt::skip]
            let input_image_args = String::from(args[2].rsplit_terminator(&['/', '.'][..]).collect::<Vec<_>>()[1]);
            let output_image = input_image_args + "_v" + &image_version_args + "_signed";

            println!("\nImage type:       mcu-image");
            println!("Curve type:       {}", args[3]);
            #[rustfmt::skip]
            println!("Input image:      {}.bin", String::from(args[2].rsplit_terminator(&['/', '.'][..]).collect::<Vec<_>>()[1]));
            #[rustfmt::skip]
            println!("Public key:       {}.der", String::from(args[4].rsplit_terminator(&['/', '.'][..]).collect::<Vec<_>>()[1]));
            println!("Image version:    {}", args[5]);
            println!("Output image:     {}.bin", output_image);

            //firmware version
            let image_version_value: u32 = args[5].parse().unwrap();
            let version: [u8; 4] = image_version_value.to_le_bytes();

            let mut mcu_image =
                fs::File::open(args[2]).expect("Need path to mcu_image binary as argument");
            mcu_image.read_to_end(&mut image_blob).unwrap();

            let mcu_image = sign_mcu_image(image_blob, args[2], sk, version);
            match mcu_image {
                Ok(val) => {
                    let file = File::create(
                        "../boards/sign_images/signed_images/{output_image}.bin"
                            .replace("{output_image}", &output_image),
                    );
                    match file {
                        Ok(mut file) => {
                            let bytes_written = file.write(val.as_slice());
                            if let Ok(val) = bytes_written {
                                println!("Output image successfully created with {} bytes.\n", val);
                            }
                        }
                        Err(e) => panic!("error: {:?}", e),
                    }
                }
                Err(_e) => {}
            }
        }
        _ => {}
    }
}

use log::{Level, Metadata, Record};
use log::{LevelFilter, SetLoggerError};

struct SimpleLogger;

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            println!("\x1b[93m[{}]\x1b[0m  {}", record.level(), record.args());
            match (record.module_path(), record.line()) {
                (Some(file), Some(line)) => {
                    println!("\t \u{2a3d} {} @ line:{}", file, line);
                }
                (Some(file), None) => println!("\t  \u{2a3d} @ {}", file),
                (_, None) => {
                    println!("... ")
                }
                (_, Some(line)) => println!("\t  \u{2a3d} {} @ line:{}", record.target(), line),
            }
        }
    }

    fn flush(&self) {}
}

pub fn log_init() -> core::result::Result<(), SetLoggerError> {
    log::set_boxed_logger(Box::new(SimpleLogger)).map(|()| log::set_max_level(LevelFilter::Info))
}

/// End-to-end check that `rustBoot::verify` accepts what `sign_mcu_image`
/// produces, and rejects it once any part is altered. This is the THEJAS32
/// port's evidence that signer and bootloader agree on the format.
#[cfg(test)]
mod thejas32_verify_tests {
    use super::*;
    use rustBoot::rbconstants::IMAGE_HEADER_SIZE;
    use rustBoot::verify::{parse_header, verify, VerifyError};

    // A fixed, valid P-256 scalar. Test-only; never used to sign real images.
    const TEST_SCALAR: [u8; 32] = [
        0x1d, 0x6b, 0x2c, 0x4e, 0x8a, 0x37, 0x91, 0x05, 0xc2, 0x7f, 0x63, 0x18, 0xa4, 0x5e, 0x0b,
        0xd9, 0x22, 0x84, 0x6f, 0x13, 0xe8, 0x50, 0x9c, 0x3a, 0x71, 0xb6, 0x0d, 0x44, 0x9f, 0x28,
        0xc5, 0x07,
    ];

    fn test_key() -> (SigningKeyType, [u8; 64]) {
        let sk = import_signing_key(CurveType::NistP256, &TEST_SCALAR).unwrap();
        let SigningKeyType::NistP256(ref k) = sk else { unreachable!() };
        let point = k.verifying_key().to_encoded_point(false);
        let mut pk = [0u8; 64];
        pk.copy_from_slice(&point.as_bytes()[1..]);
        (sk, pk)
    }

    /// Signs `fw` the way `rbsigner mcu-image` does.
    fn sign(fw: &[u8], version: u32) -> (Vec<u8>, [u8; 64]) {
        let dir = std::env::temp_dir().join(format!("rbsigner-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("fw-{version}.bin"));
        fs::write(&path, fw).unwrap();
        // sign_mcu_image asserts mtime < atime, so give the file an older mtime.
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1_700_000_000, 0))
            .unwrap();
        let (sk, pk) = test_key();
        let signed =
            sign_mcu_image(fw.to_vec(), path.to_str().unwrap(), sk, version.to_le_bytes()).unwrap();
        (signed, pk)
    }

    fn check(signed: &[u8], pk: &[u8; 64]) -> Result<u32, VerifyError> {
        let hdr: &[u8; IMAGE_HEADER_SIZE] = signed[..IMAGE_HEADER_SIZE].try_into().unwrap();
        let h = parse_header(hdr).map_err(VerifyError::Header)?;
        verify(&h, &signed[IMAGE_HEADER_SIZE..], pk).map(|()| h.version)
    }

    fn firmware() -> Vec<u8> {
        (0..5000u32).map(|i| (i * 7 + 3) as u8).collect()
    }

    #[test]
    fn signed_image_verifies_and_reports_version() {
        let (signed, pk) = sign(&firmware(), 7);
        assert_eq!(check(&signed, &pk), Ok(7));
        let hdr: &[u8; IMAGE_HEADER_SIZE] = signed[..IMAGE_HEADER_SIZE].try_into().unwrap();
        assert_eq!(parse_header(hdr).unwrap().fw_size, 5000);
    }

    #[test]
    fn flipped_firmware_bit_fails_hash() {
        let (mut signed, pk) = sign(&firmware(), 1);
        let last = signed.len() - 1;
        signed[last] ^= 0x01;
        assert_eq!(check(&signed, &pk), Err(VerifyError::HashMismatch));
    }

    #[test]
    fn changed_version_fails_hash() {
        // The version is covered by the hash, so rollback-by-editing fails.
        let (mut signed, pk) = sign(&firmware(), 1);
        signed[12] = 9;
        assert_eq!(check(&signed, &pk), Err(VerifyError::HashMismatch));
    }

    #[test]
    fn flipped_signature_bit_fails() {
        let (mut signed, pk) = sign(&firmware(), 1);
        signed[150] ^= 0x01; // inside the signature value (bytes 120..184)
        assert_eq!(check(&signed, &pk), Err(VerifyError::BadSignature));
    }

    #[test]
    fn other_key_is_reported_as_wrong_key() {
        let (signed, mut pk) = sign(&firmware(), 1);
        pk[0] ^= 0x01;
        assert_eq!(check(&signed, &pk), Err(VerifyError::WrongKey));
    }

    #[test]
    fn truncated_firmware_fails_size() {
        let (signed, pk) = sign(&firmware(), 1);
        assert_eq!(check(&signed[..signed.len() - 1], &pk), Err(VerifyError::SizeMismatch));
    }

    #[test]
    fn app_side_header_parser_reads_real_signer_output() {
        // rb_state::image::image_info is what an application uses before
        // installing an update; it must agree with what rbsigner writes.
        let (signed, _) = sign(&firmware(), 42);
        let hdr: &[u8; IMAGE_HEADER_SIZE] = signed[..IMAGE_HEADER_SIZE].try_into().unwrap();
        let info = rb_state::image::image_info(hdr).expect("parses");
        assert_eq!(info.fw_size, 5000);
        assert_eq!(info.version, 42);
        assert_eq!(rb_state::image::HEADER_SIZE, IMAGE_HEADER_SIZE);
    }

    // ------------------------------------------------------------ hybrid (LMS)

    use rustBoot::verify::verify_lms;
    use std::path::PathBuf;

    fn lms_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rbsigner-lms-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// ECDSA-signs firmware, writes it to `dir`, LMS-signs it in place.
    /// Returns (image bytes, ECDSA pubkey, LMS pubkey, LMS key path).
    fn hybrid(dir: &PathBuf, version: u32) -> (Vec<u8>, [u8; 64], Vec<u8>, PathBuf) {
        let key = dir.join("lms.key");
        let pubk = dir.join("lms.pub");
        if !key.exists() {
            lmssigner::keygen(&key, &pubk).unwrap();
        }
        let (signed, ecdsa_pk) = sign(&firmware(), version);
        let img = dir.join("img.bin");
        fs::write(&img, &signed).unwrap();
        lmssigner::append_trailer(&img, &key).unwrap();
        (fs::read(&img).unwrap(), ecdsa_pk, fs::read(&pubk).unwrap(), key)
    }

    fn check_hybrid(img: &[u8], ecdsa_pk: &[u8; 64], lms_pk: &[u8]) -> Result<u32, VerifyError> {
        let hdr: &[u8; IMAGE_HEADER_SIZE] = img[..IMAGE_HEADER_SIZE].try_into().unwrap();
        let h = parse_header(hdr).map_err(VerifyError::Header)?;
        let fw_end = IMAGE_HEADER_SIZE + h.fw_size;
        verify(&h, &img[IMAGE_HEADER_SIZE..fw_end], ecdsa_pk)?;
        verify_lms(h.digest(), &img[fw_end..], lms_pk)?;
        Ok(h.version)
    }

    #[test]
    fn lms_hybrid_image_verifies() {
        let d = lms_dir("ok");
        let (img, epk, lpk, _) = hybrid(&d, 3);
        assert_eq!(img.len(), IMAGE_HEADER_SIZE + 5000 + lmssigner::LMS_TRAILER_LEN);
        assert_eq!(lpk.len(), 60, "HSS L=1 public key");
        assert_eq!(check_hybrid(&img, &epk, &lpk), Ok(3));
    }

    #[test]
    fn lms_stripped_trailer_is_rejected() {
        let d = lms_dir("strip");
        let (img, epk, lpk, _) = hybrid(&d, 1);
        let ecdsa_only = &img[..IMAGE_HEADER_SIZE + 5000];
        assert_eq!(check_hybrid(ecdsa_only, &epk, &lpk), Err(VerifyError::LmsSignature));
    }

    #[test]
    fn lms_any_flipped_signature_byte_is_rejected() {
        let d = lms_dir("flip");
        let (img, epk, lpk, _) = hybrid(&d, 1);
        let start = IMAGE_HEADER_SIZE + 5000;
        // Every 97th byte of the trailer, including the magic and length.
        for i in (start..img.len()).step_by(97) {
            let mut bad = img.clone();
            bad[i] ^= 0x01;
            assert!(check_hybrid(&bad, &epk, &lpk).is_err(), "flip at trailer byte {} accepted", i - start);
        }
    }

    #[test]
    fn lms_other_key_is_rejected() {
        let d = lms_dir("otherkey");
        let (img, epk, _, _) = hybrid(&d, 1);
        let d2 = lms_dir("otherkey2");
        lmssigner::keygen(&d2.join("k"), &d2.join("p")).unwrap();
        let other = fs::read(d2.join("p")).unwrap();
        assert_eq!(check_hybrid(&img, &epk, &other), Err(VerifyError::LmsSignature));
    }

    #[test]
    fn lms_still_requires_a_valid_ecdsa_signature() {
        let d = lms_dir("ecdsa");
        let (mut img, epk, lpk, _) = hybrid(&d, 1);
        img[150] ^= 0x01; // inside the ECDSA signature
        assert_eq!(check_hybrid(&img, &epk, &lpk), Err(VerifyError::BadSignature));
    }

    #[test]
    fn lms_key_advances_and_never_repeats_a_one_time_key() {
        let d = lms_dir("advance");
        let key = d.join("lms.key");
        lmssigner::keygen(&key, &d.join("lms.pub")).unwrap();
        let k0 = fs::read(&key).unwrap();
        let s1 = lmssigner::sign_with_key_file(&[1; 32], &key).unwrap();
        let k1 = fs::read(&key).unwrap();
        let s2 = lmssigner::sign_with_key_file(&[2; 32], &key).unwrap();
        assert_ne!(k0, k1, "key file must change after signing");
        // HSS signature: Nspk (4) then the LMS signature, which starts with q.
        let q = |s: &[u8]| u32::from_be_bytes(s[4..8].try_into().unwrap());
        assert_eq!((q(&s1), q(&s2)), (0, 1));
    }

    #[test]
    fn lms_no_signature_without_saving_the_key() {
        // The library returns a signature only if the key update succeeds.
        let d = lms_dir("nosave");
        let key = d.join("lms.key");
        lmssigner::keygen(&key, &d.join("lms.pub")).unwrap();
        let bytes = fs::read(&key).unwrap();
        let r = hbs_lms::sign::<hbs_lms::Sha256_256>(&[7; 32], &bytes, &mut |_| Err(()), None);
        assert!(r.is_err());
    }

    #[test]
    fn lms_refuses_to_sign_twice_or_a_tampered_image() {
        let d = lms_dir("twice");
        let (img, _, _, key) = hybrid(&d, 1);
        let p = d.join("img.bin");
        fs::write(&p, &img).unwrap();
        assert!(lmssigner::append_trailer(&p, &key).is_err(), "already has a trailer");
        let mut bad = img[..IMAGE_HEADER_SIZE + 5000].to_vec();
        bad[IMAGE_HEADER_SIZE + 10] ^= 1;
        fs::write(&p, &bad).unwrap();
        assert!(lmssigner::append_trailer(&p, &key).is_err(), "digest mismatch");
    }

    /// rustBoot's own LMS verifier (rustBoot::lms) against hbs-lms, which
    /// signs: every SHA-256 LM-OTS width, several heights, and a two-level
    /// HSS. Both must accept every valid signature and reject every corrupted
    /// one.
    #[test]
    fn lms_own_verifier_agrees_with_hbs_lms() {
        use hbs_lms::{HssParameter, LmotsAlgorithm as W, LmsAlgorithm as H, Seed, Sha256_256};
        let sets: Vec<Vec<HssParameter<Sha256_256>>> = vec![
            vec![HssParameter::new(W::LmotsW1, H::LmsH5)],
            vec![HssParameter::new(W::LmotsW2, H::LmsH5)],
            vec![HssParameter::new(W::LmotsW4, H::LmsH5)],
            vec![HssParameter::new(W::LmotsW8, H::LmsH5)],
            vec![HssParameter::new(W::LmotsW4, H::LmsH10)], // what the bootloader uses
            vec![HssParameter::new(W::LmotsW2, H::LmsH5), HssParameter::new(W::LmotsW8, H::LmsH5)],
        ];
        let mut checked = 0;
        let mut hbs_lms_panics = 0;
        std::panic::set_hook(Box::new(|_| {})); // keep the expected panics quiet
        for (n, params) in sets.iter().enumerate() {
            let seed = Seed::<Sha256_256>::from([n as u8 + 1; 32]);
            let (sk, vk) = hbs_lms::keygen::<Sha256_256>(params, &seed, None).unwrap();
            let mut key = sk.as_slice().to_vec();
            let pk = vk.as_slice().to_vec();
            for m in 0..4u8 {
                let msg = [m; 32];
                let sig = hbs_lms::sign::<Sha256_256>(&msg, &key.clone(), &mut |k| {
                    key = k.to_vec();
                    Ok(())
                }, None)
                .unwrap();
                let sig = sig.as_ref();
                assert!(hbs_lms::verify::<Sha256_256>(&msg, sig, &pk).is_ok());
                assert!(rustBoot::lms::hss_verify(&msg, sig, &pk), "set {n} msg {m}: own verifier rejected a valid signature");
                for i in (0..sig.len()).step_by(sig.len() / 37 + 1) {
                    let mut bad = sig.to_vec();
                    bad[i] ^= 0x10;
                    // hbs-lms panics on some malformed signatures (it reads
                    // past the buffer when a corrupted embedded public key
                    // claims other parameters). Count that as a rejection.
                    let theirs = std::panic::catch_unwind(|| {
                        hbs_lms::verify::<Sha256_256>(&msg, &bad, &pk).is_ok()
                    });
                    if theirs.is_err() {
                        hbs_lms_panics += 1;
                    }
                    let theirs = theirs.unwrap_or(false);
                    let ours = rustBoot::lms::hss_verify(&msg, &bad, &pk);
                    assert!(!ours, "set {n} msg {m}: corrupted byte {i} accepted");
                    assert_eq!(ours, theirs, "set {n} msg {m} byte {i}: verifiers disagree");
                    checked += 1;
                }
            }
        }
        let _ = std::panic::take_hook();
        println!("{checked} corrupted signatures: both verifiers rejected all; hbs-lms panicked on {hbs_lms_panics}");
        assert!(checked > 500);
    }

    #[test]
    fn lms_trailer_size_matches_the_bootloader() {
        // rb-state's constant sizes the trailer the bootloader reads and the
        // app installer writes; it must equal what this signer appends.
        assert_eq!(lmssigner::LMS_TRAILER_LEN, rb_state::image::LMS_TRAILER_LEN);
    }

    #[test]
    fn lms_keygen_never_overwrites() {
        let d = lms_dir("overwrite");
        lmssigner::keygen(&d.join("k"), &d.join("p")).unwrap();
        assert!(lmssigner::keygen(&d.join("k"), &d.join("p2")).is_err());
    }

    #[test]
    fn bad_magic_is_rejected() {
        let (mut signed, pk) = sign(&firmware(), 1);
        signed[0] = 0;
        assert!(matches!(check(&signed, &pk), Err(VerifyError::Header(_))));
    }
}
