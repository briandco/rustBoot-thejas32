mod curve;
mod fitsigner;
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

    #[test]
    fn bad_magic_is_rejected() {
        let (mut signed, pk) = sign(&firmware(), 1);
        signed[0] = 0;
        assert!(matches!(check(&signed, &pk), Err(VerifyError::Header(_))));
    }
}
