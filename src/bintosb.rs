#![allow(unused_imports)]
use std::fs;

use serde::{Deserialize, Serialize};

use crate::error::Result;

use nom::{
    branch::alt,
    bytes::complete::{
        tag, take, take_while_m_n,
    },
    combinator::{
        map, value, verify,
    },
    multi::{
        fill,
    },
    number::complete::{
        u8, le_u16, be_u32, le_u32, le_u64, le_u128,
    },
    sequence::tuple,
};

pub mod crc;

const IMAGE_ALIGNMENT: usize = 4;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub root_cert_secret_key: String,
    pub root_cert_filenames: [String; 4],
    pub image: String,
    pub signed_image: String,
    // pub factory: FactoryArea,
    // pub field: FieldAreaPage,
    // pub keystore: Keystore,
}

#[derive(Clone, Copy, Debug, Hash)]
pub enum Filetype {
    Elf,
    UnsignedBin,
    SignedBin,
    Sb20,
    Sb21,
}

pub fn sniff(file: &[u8]) -> Result<Filetype> {
    Ok(match &file[..4] {
        // ELF
        b"\x7fELF" => Filetype::Elf,
        // BIN
        // this criterion is a bit unstable I guess.
        // https://interrupt.memfault.com/blog/zero-to-main-1
        // firmware starts with SP (4b) then PC (4B)
        // maybe: fallback to viewing as "bin" if not ELF or SB?
        &[0x00, 0x00, 0x04, 0x20] => {
            match &file[0x20..0x24] {
                &[0x00, 0x00, 0x00, 0x00] => Filetype::UnsignedBin,
                _ => Filetype::SignedBin,
            }
        }
        _ => {
            match &file[20..24] {
                // SB2.0 or SB2.1
                b"STMP" => {
                    match &file[52..56] {
                        b"sgtl" => Filetype::Sb21,
                        _ => Filetype::Sb20,
                    }
                }
                // out of ideas
                _ => {
                    return Err(anyhow::anyhow!("no clue"));
                }
            }
        }
    })
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BootTag {
    Nop = 0,
    Tag = 1,
    Load = 2,
    Fill = 3,
    Jump = 4,
    Call = 5,
    ChangeBootMode = 6,
    Erase = 7,
    Reset = 8,
    MemoryEnable = 9,
    ProgramPersistentBits = 0xA,
    CheckFirmwareVersion = 0xB,
    KeystoreToNonvolatile = 0xC,
    KeystoreFromNonvolatile = 0xD,
}

// struct boot_command_t
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawBootCommand {
    checksum: u8,
    tag: u8,
    flags: u16,
    address: u32,
    count: u32,
    data: u32,
}

impl RawBootCommand {
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut buffer = Vec::new();
        buffer.push(0);
        buffer.push(self.tag);
        buffer.extend_from_slice(self.flags.to_le_bytes().as_ref());
        buffer.extend_from_slice(self.flags.to_le_bytes().as_ref());
        buffer.extend_from_slice(self.count.to_le_bytes().as_ref());
        buffer.extend_from_slice(self.data.to_le_bytes().as_ref());
        let checksum = buffer[1..].iter().fold(0x5au8, |acc, x| acc.wrapping_add(*x));
        buffer[0] = checksum;
        use core::convert::TryInto;
        buffer.try_into().unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> nom::IResult<&[u8], Self, ()> {
        let (i, (
            checksum,
            tag,
            flags,
            address,
            count,
            data,
        )) = tuple((
            u8,
            u8,
            le_u16,
            le_u32,
            le_u32,
            le_u32,
        ))(bytes)?;

        // by previous, bytes.len() >= 16
        info!("raw boot command: {}", hex_str!(&bytes[..16]));
        let calculated_checksum = bytes[1..16].iter().fold(0x5au8, |acc, x| acc.wrapping_add(*x));
        assert_eq!(calculated_checksum, checksum);

        Ok((i, Self { checksum, tag, flags, address, count, data }))
    }
}

#[derive(Clone, Debug, PartialEq)]
// The LPC55xx ROM loader provides the support for the following bootloader commands:
// * WriteMemory, FillMemory, ConfigureMemory, FlashEraseAll, FlashEraseRegion,
// SB 2.1 introduces two new commands that can be used to prevent firmware roll-back:
// * SecureFirmwareVersion, NonsecureFirmwareVersion
pub enum BootCommand {
    // example: 5A|00|0000|00000000|00000000|00000000
    Nop,
    // example: F3|01|0180|00000000|B2640000|01000000
    Tag { last: bool, tag: u32, flags: u32, cipher_blocks: u32 },
    // example: 8F|02|0000|00000000|00020000|A6D9585A
    Load { address: u32, data: Vec<u8> },
    // example?
    /// See ELFTOSB document for explanations of what is supposed to happen when
    /// address is not on a word boundary.
    ///
    /// In any case, if a byte is supposed to be repeated, it must be replicated
    /// four times in the `pattern`, e.g. "fill with 0xF1" => pattern = `0xf1f1_f1f1`.
    Fill { address: u32, bytes: u32, pattern: u32 },
    // example?
    EraseAll,
    // example: 01|07|0000|00000000|00980800|00000000
    // NB: this command is interpreted as "erase all flash sectors that intersect with the
    // specified region"
    EraseRegion { address: u32, bytes: u32 },
    CheckSecureFirmwareVersion { version: u32 },
    CheckNonsecureFirmwareVersion { version: u32 },
}

impl BootCommand {
    pub fn to_bytes(&self) -> Vec<u8> {
        use BootCommand::*;
        let mut cmd: RawBootCommand = Default::default();
        match self {
            Nop => {
                cmd.tag = BootTag::Nop as u8;
                Vec::from(cmd.to_bytes().as_ref())
            }
            Tag { last, tag, flags, cipher_blocks } => {
                cmd.tag = BootTag::Tag as u8;
                if *last {
                    cmd.flags = 1;
                }
                cmd.address = *tag;
                cmd.data = *flags;
                cmd.count = *cipher_blocks;
                Vec::from(cmd.to_bytes().as_ref())
            }
            Load { address, data } => {
                cmd.tag = BootTag::Load as u8;
                cmd.count = data.len() as u32;
                // this takes advantage of the fact that our crc32
                // adds "padding till multiple of 16 bytes with zeros"
                // to the CRC calculation.
                cmd.data = crc::crc32(data);
                let blocks = (data.len() + 15) / 16;
                // let padding = blocks*16 - data.len();
                let mut vec = Vec::from(cmd.to_bytes().as_ref());
                vec.extend_from_slice(data.as_ref());
                // add padding
                // NB: NXP says to fill with random bytes, I don't see the point.
                // We're not actually encrypting anyway, and AES is supposed to be a... cipher ;)
                vec.resize(32 + 16*blocks, 0);
                vec
            }
            EraseAll => {
                cmd.tag = BootTag::Erase as u8;
                cmd.flags = 1;
                Vec::from(cmd.to_bytes().as_ref())
            }
            EraseRegion { address, bytes } => {
                cmd.tag = BootTag::Erase as u8;
                cmd.address = *address;
                cmd.count = *bytes;
                Vec::from(cmd.to_bytes().as_ref())
            }
            CheckSecureFirmwareVersion { version } => {
                cmd.tag = BootTag::CheckFirmwareVersion as u8;
                cmd.count = *version;
                Vec::from(cmd.to_bytes().as_ref())
            }
            CheckNonsecureFirmwareVersion { version } => {
                cmd.tag = BootTag::CheckFirmwareVersion as u8;
                cmd.address = 1;
                cmd.count = *version;
                Vec::from(cmd.to_bytes().as_ref())
            }
            _ => todo!(),
        }
    }
    pub fn from_bytes(bytes: &[u8]) -> nom::IResult<&[u8], Self, ()> {
        let (i, raw) = RawBootCommand::from_bytes(bytes)?;
        Ok(match raw.tag {
            // BootTag::Nop => {
            0 => {
                // todo? check everything zero except checksum
                (i, Self::Nop)
            }
            // BootTag::Tag => {
            1 => {
                (i, Self::Tag {
                    last: (raw.flags & 1) != 0,
                    tag: raw.address,
                    flags: raw.data,
                    cipher_blocks: raw.count,
                })
            }
            // BootTag::Load => {
            2 => {
                let blocks = (raw.count as usize + 15) / 16;
                let (i, data_ref) = take(blocks * 16)(i)?;
                let data = Vec::from(&data_ref[..raw.count as usize]);
                if raw.count as usize != data_ref.len() {
                    info!("surplus random bytes skipped when reading: {}", hex_str!(&data_ref[raw.count as usize..]));
                }
                // verify "CRC-32" calculation:
                // raw.data == CRC over entire contents of `data_ref`, including padding
                let calculated_crc = crc::crc32(data_ref);
                assert_eq!(calculated_crc, raw.data);
                (i, Self::Load {
                    address: raw.address,
                    // bytes: data.len(),
                    data,
                })
            }
            // BootTag::Fill => {
            3 => {
                (i, Self::Fill {
                    address: raw.address,
                    bytes: raw.count,
                    pattern: raw.data,
                })
            }
            // BootTag::Erase => {
            7 => {
                let erase_all = (raw.flags & 1) != 0;
                let disable_flash_security_state = (raw.flags & 2) != 0;
                // not supported yet
                assert!(!disable_flash_security_state);
                let memory_controller_id = (raw.flags >> 8) & 0b1111;
                // expect "internal" flash"
                assert_eq!(memory_controller_id, 0x0);

                if erase_all {
                    // raw.address and raw.count are ignored
                    (i, Self::EraseAll)
                } else {
                    (i, Self::EraseRegion {
                        address: raw.address,
                        bytes: raw.count,
                    })
                }
            }
            // BootTag::CheckFirmwareVersion => {
            0xB => {
                // header.m_address = ENDIAN_HOST_TO_LITTLE_U32((uint32_t)m_versionType);
                // header.m_count = ENDIAN_HOST_TO_LITTLE_U32(m_version);
                // SecureVersion = 0x0,
                // NonSecureVersion = 0x1,
                let nonsecure_version = (raw.address & 1) != 0;
                // header.m_address = ENDIAN_HOST_TO_LITTLE_U32((uint32_t)m_versionType);
                // header.m_count = ENDIAN_HOST_TO_LITTLE_U32(m_version);
                if nonsecure_version {
                    (i, Self::CheckNonsecureFirmwareVersion { version: raw.count })
                } else {
                    (i, Self::CheckSecureFirmwareVersion { version: raw.count })
                }
            }
            _ => todo!("implement other boot commands"),
        })
    }
}

pub fn show(filename: &str) -> Result<Vec<u8>> {
    let data = fs::read(filename)?;
    trace!("filename: {}", filename);
    trace!("filesize: {}B", data.len());

    let filetype = sniff(&data)?;
    trace!("filetype: {:?}", filetype);

        use nom::{
            branch::alt,
            bytes::complete::{
                tag, take, take_while_m_n,
            },
            combinator::{
                value, verify,
            },
            multi::{
                fill,
            },
            number::complete::{
                u8, le_u16, le_u32, le_u64, le_u128,
            },
        };

    match filetype {
        Filetype::Sb21 => {
            let (i, header) = Sb2Header::inner_from_bytes(&data)?;//.unwrap();//.1;//.map_err(|_| anyhow::anyhow!("could not parse SB2 file"))?.1;
            let (i, digest_hmac) = take::<_, _, ()>(32u8)(i)?;
            let (i, keyblob) = Keyblob::from_bytes(i)?;
            let (i, certificate_block_header) = FullCertificateBlockHeader::from_bytes(i)?;
            let (i, certificate_length) = le_u32::<_, ()>(i).unwrap();
            let (i, certificate_data) = take::<_, _, ()>(certificate_length)(i)?;
            let (i, rot_key_hashes) = take::<_, _, ()>(128usize)(i)?;
            let (i, signature) = take::<_, _, ()>(256usize)(i)?;

            // the weird sectionAllignment (sic!)
            info!("SB2 header: \n{:#?}", &header);
            info!("HMAC:       \n{:?}", &digest_hmac);
            info!("keyblob:    \n{:?}", &keyblob);
            info!("CTH:        \n{:?}", &certificate_block_header);

            let certificate = match x509_parser::parse_x509_certificate(certificate_data) {
                Ok((rem, cert)) => {
                    println!("remainder: {}", hex_str!(rem));
                    // assert!(rem.is_empty());
                    assert_eq!(cert.tbs_certificate.version, x509_parser::x509::X509Version::V3);
                    cert
                }
                _ => { panic!("invalid certificate"); }
            };
            // info!("cert: \n{:?}", &certificate);

            // now let's verify the signature
            let signed_data_length = 16*(6 + 2 + 5 + 2) + 4 + certificate_length + 128;
            // let signed_data_length = 0x5f0;
            println!("end of cert data: {:>16x}", hex_str!(&certificate_data));
            println!("signed_data_length: 0x{:x}", signed_data_length);

            fn sha256(data: &[u8]) -> [u8; 32] {
                use sha2::Digest;
                let mut hasher = sha2::Sha256::new();
                hasher.update(&data);
                let mut digest = [0u8; 32];
                digest.copy_from_slice(&hasher.finalize());
                digest
            }

            let signed_data_hash = sha256(&data[..signed_data_length as usize]);
            println!("data hash: {}", hex_str!(&signed_data_hash, 4));

            let spki = certificate.tbs_certificate.subject_pki;
            trace!("alg: {:?}", spki.algorithm.algorithm);
            assert_eq!(oid_registry::OID_PKCS1_RSAENCRYPTION, spki.algorithm.algorithm);

            println!("rsa pub key: {:?}", &spki.subject_public_key.data);
            let public_key = rsa::RSAPublicKey::from_pkcs1(&spki.subject_public_key.data).expect("can parse public key");
            println!("signature: {}", hexstr!(&signature));
            let padding_scheme = rsa::PaddingScheme::new_pkcs1v15_sign(Some(rsa::Hash::SHA2_256));
            use rsa::PublicKey;
            public_key.verify(padding_scheme, &signed_data_hash, signature).expect("signature valid");
            // let signature = secret_key.sign(padding_scheme, &hashed_image).expect("signatures work");

            let calculated_boot_tag_offset_bytes = signed_data_length + 256;
            assert_eq!(calculated_boot_tag_offset_bytes, header.boot_tag_offset_blocks * 16);

            // alright, BootTag, Hmac, Section
            //
            // here's what happens:
            // - encryption is weird... (big-endian AES-CTR, but with nonce modified by adding
            // block number to little-endian encoding of last nonce-value)
            //
            // - first: encrypted boot tag
            // - then: unencrypted HMAC of encrypted boot tag
            // - then: unencrypted HMAC of encrypted section data (commands and their data)
            // - then: encrypted section data
            //
            // the digest HMAC at the top after image header is HMAC(first HMAC || second HMAC)
            //
            //
            let boot_tag_offset_blocks = header.boot_tag_offset_blocks;

            fn decipher(ciphertext: &[u8], dek: [u8; 32], nonce: [u32; 4], offset_blocks: u32) -> Vec<u8> {
                type Aes256Ctr = ctr::Ctr32BE<aes::Aes256>;
                use ctr::cipher::SyncStreamCipher;
                use ctr::cipher::stream::NewStreamCipher;

                let mut plaintext = Vec::from(ciphertext);

                for (i, chunk) in plaintext.chunks_mut(16).enumerate() {
                    // see SB2Image.cpp:229
                    let nonce3_offset = offset_blocks + (i as u32);
                    let mut nonce2 = [0u8; 16];
                    nonce2[..4].copy_from_slice(nonce[0].to_le_bytes().as_ref());
                    nonce2[4..8].copy_from_slice(&nonce[1].to_le_bytes().as_ref());
                    nonce2[8..12].copy_from_slice(&nonce[2].to_le_bytes().as_ref());
                    nonce2[12..16].copy_from_slice(&(nonce[3] + nonce3_offset).to_le_bytes().as_ref());
                    let mut cipher = Aes256Ctr::new(dek.as_ref().into(), nonce2.as_ref().into());
                    cipher.apply_keystream(chunk);
                }

                plaintext
            }

            fn hmac(mac_key: [u8; 32], data: &[u8]) -> [u8; 32] {
                use sha2::Sha256;
                use hmac::{Hmac, Mac, NewMac};

                type HmacSha256 = Hmac<Sha256>;

                let mut mac = HmacSha256::new_varkey(&mac_key).unwrap();
                mac.update(data);
                let result = mac.finalize();

                let mut digest = [0u8; 32];
                digest.copy_from_slice(&result.into_bytes());
                digest
            }

            let (i, enciphered_boot_tag) = take::<_, _, ()>(16u8)(i)?;
            let calculated_boot_tag_hmac = hmac(keyblob.mac, &enciphered_boot_tag);

            let deciphered_boot_tag = decipher(
                enciphered_boot_tag,
                keyblob.dek, header.nonce,
                header.boot_tag_offset_blocks,
            );

            let (_, boot_tag) = BootCommand::from_bytes(&deciphered_boot_tag)?;
            println!("boot tag: {:?}", &boot_tag);
            // TODO? check cipher blocks

            let (i, hmac_table) = take::<_, _, ()>(64u8)(i)?;

            let (_, (boot_tag_hmac, section_hmac)) = tuple((
                take::<_, _, ()>(32u8),
                take::<_, _, ()>(32u8),
            ))(hmac_table)?;

            assert_eq!(boot_tag_hmac, calculated_boot_tag_hmac);

            // let (i, section_hmac) = take::<_, _, ()>(32u8)(i)?;

            let enciphered_section = i;

            let calculated_section_hmac = hmac(keyblob.mac, enciphered_section);
            assert_eq!(section_hmac, calculated_section_hmac);

            let deciphered_section = decipher(
                enciphered_section,
                keyblob.dek, header.nonce,
                header.boot_tag_offset_blocks + 5,
            );

            let calculated_digest_hmac = hmac(keyblob.mac, hmac_table);
            assert_eq!(digest_hmac, calculated_digest_hmac);

            let mut i = deciphered_section.as_ref();
            while true {
                let (j, command) = BootCommand::from_bytes(i)?;
                i = j;
                println!("command: {:?}", &command);
                if i.is_empty() {
                    break;
                }
            }

//             let
//             let mut nonce = header.nonce.clone();
//             // see SB2Image.cpp:229
//             println!("nonce3 = {:x}", &nonce[3]);
//             nonce[3] += 111;
//             let mut nonce2 = [0u8; 16];
//             nonce2[..4].copy_from_slice(nonce[0].to_le_bytes().as_ref());
//             nonce2[4..8].copy_from_slice(&nonce[1].to_le_bytes().as_ref());
//             nonce2[8..12].copy_from_slice(&nonce[2].to_le_bytes().as_ref());
//             nonce2[12..16].copy_from_slice(&nonce[3].to_le_bytes().as_ref());
//             println!("nonce2 = {}", hex_str!(&nonce2));
//             println!("dek = {}", hex_str!(&keyblob.dek));
//             println!("mac = {}", hex_str!(&keyblob.mac));


//             println!("encrypted boot tag: {}", hexstr!(&i[..16]));
//             println!("start of hmac: {}", hexstr!(&i[16..48]));
//             println!("part 2 ofhmac: {}", hexstr!(&i[48..80]));
//             let mut section = Vec::from(i);

//             for (i, chunk) in section.chunks_mut(16).enumerate() {
//                 let mut nonce = header.nonce.clone();
//                 // see SB2Image.cpp:229
//                 // println!("nonce3 = {:x}", &nonce[3]);
//                 nonce[3] += 111 + (i as u32);
//                 let mut nonce2 = [0u8; 16];
//                 nonce2[..4].copy_from_slice(nonce[0].to_le_bytes().as_ref());
//                 nonce2[4..8].copy_from_slice(&nonce[1].to_le_bytes().as_ref());
//                 nonce2[8..12].copy_from_slice(&nonce[2].to_le_bytes().as_ref());
//                 nonce2[12..16].copy_from_slice(&nonce[3].to_le_bytes().as_ref());
//                 use ctr::cipher::SyncStreamCipher;
//                 let mut cipher = Aes256Ctr::new(keyblob.dek.as_ref().into(), nonce2.as_ref().into());
//                 cipher.apply_keystream(chunk);
//             }

//             println!("section length: {}", section.len());

//             println!("start of section data: \n{:<320}", hex_str!(&section, 16, sep: "\n"));
//             println!("end of section data: \n{:>544}", hex_str!(&section, 16, sep: "\n"));

//             let i: &[u8] = &section;//.as_ref();

//             let (i, boot_tag) = take::<_, _, ()>(16u8)(i)?;
//             // let mut boot_tag = [0u8; 16];
//             // boot_tag.copy_from_slice(boot_tag_ref);
//             // println!("encrypted boot tag: {}", hex_str!(&boot_tag));
//             // cipher.apply_keystream(&mut boot_tag);

//             // expect: boot_tag[1] = tag = 0x01 (ROM_TAG_CMD)
//             println!("boot tag: {}", hex_str!(boot_tag));
//             // get:
//             // checksum: 0xf3 (= hex((1+1+0x80+0xb2+0x64+1+0x5a) % 256) )
//             // tag: 0x01
//             // flags: 0x8001
//             // address: 0x0000_0000
//             // count: 0x0000_64B2
//             // data: 0x0000_0001

//             // looks like section HMAC table is 64 bytes.
//             // however, a SHA256 hash is only 32 bytes.
//             let (i, section_hmac) = take::<_, _, ()>(64u8)(i)?;
//             println!("section HMAC: {}", hex_str!(section_hmac));

//             let mut hasher = sha2::Sha256::new();
//             hasher.update(section_hmac);
//             let hashed_hmac = hasher.finalize();
//             println!("hashed hmac: {}", hex_str!(&hashed_hmac));
//             println!("HMAC: {}", hex_str!(digest_hmac));

//             let (i, first_tag) = take::<_, _, ()>(16u8)(i)?;
//             println!("first tag: {}", hex_str!(first_tag));
//             let (i, first_tag) = take::<_, _, ()>(16u8)(i)?;
//             println!("first tag: {}", hex_str!(first_tag));
//             let (i, first_tag) = take::<_, _, ()>(16u8)(i)?;
//             println!("first tag: {}", hex_str!(first_tag));
//             let (i, first_tag) = take::<_, _, ()>(16u8)(i)?;
//             println!("first tag: {}", hex_str!(first_tag));
        }
        _ => {}
    }

    todo!();
}

pub fn sign(config_filename: &str) -> Result<Vec<u8>> {
    let config = fs::read_to_string(config_filename)?;
    let config: Config = toml::from_str(&config)?;
    let plain_image = fs::read(config.image)?;
    let der = fs::read(&config.root_cert_filenames[0])?;

    let sk_data = fs::read_to_string(&config.root_cert_secret_key)?;
    // do this instead:
    // https://docs.rs/rsa/0.3.0/rsa/struct.RSAPrivateKey.html?search=#example
    let der_bytes = pem_parser::pem_to_der(&sk_data);
    // use std::io::BufRead;
    // let der_encoded = sk_data
    //     .lines()
    //     .filter(|line| !line.starts_with("-"))
    //     .fold(String::new(), |mut data, line| {
    //         data.push_str(&line);
    //         data
    //     });
    // let der_bytes = base64::decode(&der_encoded).expect("failed to decode base64 content");
    let sk = rsa::RSAPrivateKey::from_pkcs1(&der_bytes)?;

    let rotkh = crate::rotkh::rot_key_hashes(&config.root_cert_filenames)?;
    let signed_image = assemble_signed_image(
        &plain_image,
        &der,
        rotkh,
        &sk,
    );
    fs::write(&config.signed_image, &signed_image)?;
    Ok(signed_image)
}


fn pad_alignment(data: &mut Vec<u8>) {
    let size = data.len();

    // let padding = if (size % IMAGE_ALIGNMENT) > 0 {
    //     IMAGE_ALIGNMENT - (size % IMAGE_ALIGNMENT)
    // } else {
    //     0
    // };
    // let aligned_size = size + padding;

    // dumb C tricks for above
    // let aligned_size = (size + (IMAGE_ALIGNMENT - 1)) & (-(IMAGE_ALIGNMENT as isize) as usize);
    let aligned_size = (size + (IMAGE_ALIGNMENT - 1)) & (!(IMAGE_ALIGNMENT - 1));
    data.resize(aligned_size, 0);
}

fn padded_alignment(data: &[u8]) -> Vec<u8> {
    let mut data = Vec::from(data);
    pad_alignment(&mut data);
    data
}

// UM11126, Chap. 6, Table 172, "Image header"
fn modify_header(padded_image: &mut Vec<u8>, padded_certificate_length: usize) -> usize {
    let image_size = padded_image.len();

    let non_image_size =
        // certificate block header
        32 +
        // certificate table size (each is u32(certificate size) + certificate, we have only one)
        (4 + padded_certificate_length) +
        // 4x ROT key SHA256 hash
        4*32 +
        // RSA2K signature
        256;

    let total_image_size = image_size + non_image_size;

    // 0x20: total image size
    padded_image[0x20..][..4].copy_from_slice((total_image_size as u32).to_le_bytes().as_ref());
    // 0x24: image type "SPT" = [XIP Signed, TZ disabled, 0, 0-]
    // This doesn't seem to match UM 11126, Chap. 7, Table 183 at all :)
    padded_image[0x24..][..4].copy_from_slice(&[0x04, 0x40, 0x00, 0x00]);
    // "header offset", i.e. image size
    padded_image[0x28..][..4].copy_from_slice((image_size as u32).to_le_bytes().as_ref());

    total_image_size
}

pub struct CertificateBlockHeader {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
}

fn extend16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(value.to_le_bytes().as_ref());
}

fn extend32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(value.to_le_bytes().as_ref());
}

#[allow(dead_code)]
fn extend64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(value.to_le_bytes().as_ref());
}

fn extendu(bytes: &mut Vec<u8>, value: usize) {
    // on desktop, usize is u64
    bytes.extend_from_slice((value as u32).to_le_bytes().as_ref());
}

fn certificate_block_header_bytes(total_image_length: usize, aligned_cert_length: usize, build_number: u32) -> Vec<u8> {
    let mut bytes = Vec::new();

    // UM 11126, Chap 7, Table 185

    // actual header: first 16/32 bytes
    bytes.extend_from_slice(b"cert");
    // headerMajorVersion = 1
    extend16(&mut bytes, 1);
    // headerMinorVersion = 0
    extend16(&mut bytes, 0);
    // header length = 0x20
    extend32(&mut bytes, 0x20);
    // RFU
    extend32(&mut bytes, 0);

    // actual header: second 16/32 bytes
    // "for downgrade protection"
    // grep for FOUR_CHAR_CODE
    extend32(&mut bytes, build_number);
    extendu(&mut bytes, total_image_length);
    print!("set length to {:x}", total_image_length);

    let certificates: u32 = 1;
    // one certificate
    extend32(&mut bytes, certificates);
    // 0x480 = 1152
    extendu(&mut bytes, aligned_cert_length + 4);

    assert_eq!(32, bytes.len());

    bytes
}

fn assemble_signed_image(plain_image: &[u8], certificate_der: &[u8], rot_key_hashes: [[u8; 32]; 4], secret_key: &rsa::RSAPrivateKey) -> Vec<u8> {

    let mut image = padded_alignment(plain_image);
    let certificate = padded_alignment(certificate_der);

    let total_image_size = modify_header(&mut image, certificate.len());
    println!("{:x}", total_image_size);

    let build_number = 1;
    let certificate_block_header = certificate_block_header_bytes(
        // total image size sans signature
        total_image_size - 256,
        certificate.len(),
        build_number,
    );
    // certificate block header
    image.extend_from_slice(&certificate_block_header);

    // certificate block
    extendu(&mut image, certificate.len());
    image.extend_from_slice(&certificate);

    // ROT key hash table
    for rot_key_hash in rot_key_hashes.iter() {
        image.extend_from_slice(rot_key_hash.as_ref());
    }
    // signature
    let padding_scheme = rsa::PaddingScheme::new_pkcs1v15_sign(Some(rsa::Hash::SHA2_256));
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(&image);
    let hashed_image = hasher.finalize();
    let signature = secret_key.sign(padding_scheme, &hashed_image).expect("signatures work");
    assert_eq!(256, signature.len());
    image.extend_from_slice(&signature);

    image

}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

fn version_entry(input: &[u8]) -> nom::IResult<&[u8], u16, ()> {
  let literal_u16 = |x: u16| verify(le_u16, move |y| *y == x);
  map(
    tuple((le_u16, literal_u16(0))),
    // take_while_m_n(2, 2, is_hex_digit),
    |(entry, _padding)| entry
  )(input)
}

fn version(i: &[u8]) -> nom::IResult<&[u8], Version, ()> {
    let mut entries = [0u16; 3];
    let (i, ()) = fill(version_entry, &mut entries)(i)?;
    Ok((i, Version { major: entries[0], minor: entries[1], patch: entries[2] }))
}
// fn from_hex(input: &str) -> Result<u8, std::num::ParseIntError> {
//   u8::from_str_radix(input, 16)
// }


const FEISTEL_ROUNDS: usize = 5;

// #[derive(Debug)]
// pub struct Aes256KeyWrap {
//     aes: Aes256,
// }

// impl Aes256KeyWrap {
//     pub const KEY_BYTES: usize = 32;
//     pub const MAC_BYTES: usize = 8;

//     pub fn new(key: &[u8; Self::KEY_BYTES]) -> Self {
//         Aes256KeyWrap {
//             aes: aes::Aes256::new(key.into()),
//         }
//     }
// }

fn aes_unwrap(key: &[u8; 32], wrapped: &[u8]) -> Vec<u8> {
    use core::convert::TryInto;
    if key.len() % 8 != 0 {
        // return Err(());
        todo!();
    }
    use aes::{BlockCipher, NewBlockCipher};
    use aes::cipher::generic_array::GenericArray;
    let aes = aes::Aes256::new(key.into());
    let n = (wrapped.len() as u64) / 8 - 1;
    let mut A = u64::from_be_bytes(wrapped[..8].try_into().unwrap());
    let mut R = Vec::new();
    // to keep NIST indices, never used
    R.push(0);
    for (i, C) in (1..=n).zip(wrapped.chunks(8).skip(1)) {
        R.push(u64::from_be_bytes(C.try_into().unwrap()));
    }
    let mut B = [0u8; 16];
    for j in (0..=5).rev() {
        for i in (1..=n).rev() {
            let t = (n*j + i) as u64;
            B[..8].copy_from_slice(&(A ^t).to_be_bytes());
            B[8..].copy_from_slice(&R[i as usize].to_be_bytes());
            // let mut B = ((A ^ t) | R[i as usize]).to_be_bytes();
            aes.decrypt_block(GenericArray::from_mut_slice(&mut B));
            A = u64::from_be_bytes(B[..8].try_into().unwrap());
            R[i as usize] = u64::from_be_bytes(B[8..].try_into().unwrap());
        }
    }
    println!("A = {}", A);
    println!("A = {}", hex_str!(&A.to_be_bytes()));
    // A ?= 'A6 A6 A6 A6 A6 A6 A6 A6'
    assert_eq!(A, 12008468691120727718);
    let mut P = Vec::new();
    for i in 1..=n {
        P.extend_from_slice(&R[i as usize].to_be_bytes());
    }
    P
}

#[derive(Clone, Debug)]
pub struct Keyblob {
    dek: [u8; 32],
    mac: [u8; 32],
}

impl Keyblob {
    const SBKEK: &'static [u8; 32] = b"\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA\xAA";

    fn from_bytes(i: &[u8]) -> nom::IResult<&[u8], Self, ()> {
        // if i.len() != 0x60 {
        //     return Err(anyhow::anyhow!("wrong size for SB2 header"));
        // }
        use nom::{
            branch::alt,
            bytes::complete::{
                tag, take, take_while_m_n,
            },
            combinator::{
                value, verify,
            },
            multi::{
                fill,
            },
            number::complete::{
                u8, le_u16, le_u32, le_u64, le_u128,
            },
        };

        println!("{}", hex_str!(Self::SBKEK));
        let (i, encapsulated) = take(72u8)(i)?;
        let (i, _) = take(8u8)(i)?;
        println!("{}", hex_str!(encapsulated));
        let unwrapped = b"\x15\x17\xba\x1c\x12\xe2:!\r\xeb\xf1p7\xc3=\x17\x06e:S\xd2\xb7\xf4P-\x11\x01-m\x0f\x8d\x8ey\x17\xd9\xc6x\xc7\xb0\x18\xd9\xf8\x17\xb4fws\"_\xb2\x10\xd3\x9f\x10\xa2\xb5K\x18\xd9\x1d\x1c\xd6\"\x85";
        // println!("wrapped: \n{}", hex_str!(
        //     &Aes256KeyWrap::new(Self::SBKEK).encapsulate(unwrapped).unwrap()));


        // let keywrap = rust_aes_keywrap::Aes256KeyWrap::new(Self::SBKEK);
        // let decapsulated = keywrap.decapsulate(encapsulated, 64).unwrap();
        let decapsulated = aes_unwrap(Self::SBKEK, encapsulated);//apsulate(encapsulated, 64).unwrap();
        // let mut nonce = [0u32; 4];
        println!("decapsulated = {}", hex_str!(&decapsulated));
        let mut dek = [0u8; 32];
        let mut mac = [0u8; 32];
        dek.copy_from_slice(&decapsulated[..32]);
        mac.copy_from_slice(&decapsulated[32..]);
        Ok((i, Self { dek, mac }))
    }
}


/// full size: 0x60 = 96 bytes
#[derive(Clone, Debug)]
pub struct Sb2Header {
    nonce: [u32; 4],
    // nonce: [u8; 16],
    header_version_minor: u8,
    flags: u16,
    image_size_blocks: u32,
    boot_tag_offset_blocks: u32,
    boot_section_id: u32,
    certificate_block_header_offset_bytes: u32,
    header_size_blocks: u16,
    keyblob_offset_blocks: u16,
    keyblob_size_blocks: u16,
    max_section_mac_count: u16,
    // flags: Sb2Flags,
    // image_size: usize,
    // boot_tag_offset: usize,
    // certificate_offset: usize,
    // keyblob_offset,
    // max_hmac_table_entries: u16,
    timestamp_microseconds_since_millenium: u64,
    product_version: Version,
    component_version: Version,
    build_number: u32,
}

    // struct certificate_block_header_t {
    //     uint8_t  signature[4];                  //!< Always set to 'cert'
    //     uint16_t headerMajorVersion;            //!< Set to 1
    //     uint16_t headerMinorVersion;            //!< Set to 0
    //     uint32_t headerLengthInBytes;           //!< Starting from the signature and not including the certificate table.
    //     uint32_t flags;                         //!< Reserved for future use.
    //     uint32_t buildNumber;                   //!< Build number of the user code. Allows user to prevent reverting to old versions
    //     uint32_t totalImageLengthInBytes;       //!< Length in bytes of the signed data
    //     uint32_t certificateCount;              //!< Must be greater than 0
    //     uint32_t certificateTableLengthInBytes; //!< Total length in bytes of the certificate table
    // };

#[derive(Clone, Debug)]
pub struct FullCertificateBlockHeader {
    header_length_in_bytes: u32,
    build_number: u32,
    total_image_length_in_bytes: u32,
    certificate_count: u32,
    certificate_table_length_bytes: u32,
}

impl FullCertificateBlockHeader {
    fn from_bytes(i: &[u8]) -> nom::IResult<&[u8], Self, ()> {
        // let literal_u8 = |x: u8| verify(u8, move |y| *y == x);
        let literal_u16 = |x: u16| verify(le_u16, move |y| *y == x);
        let literal_u32 = |x: u32| verify(le_u32, move |y| *y == x);

        let (i, _signature) = tag("cert")(i)?;
        let (i, _header_major_version) = literal_u16(1)(i)?;
        let (i, _header_minor_version) = literal_u16(0)(i)?;
        let (i, header_length_in_bytes) = le_u32(i)?;
        let (i, _flags) = le_u32(i)?;
        let (i, build_number) = le_u32(i)?;
        let (i, total_image_length_in_bytes) = le_u32(i)?;
        let (i, certificate_count) = literal_u32(1)(i)?;
        let (i, certificate_table_length_bytes) = le_u32(i)?;

        Ok((i, Self {
            header_length_in_bytes,
            build_number,
            total_image_length_in_bytes,
            certificate_count,
            certificate_table_length_bytes,
        }))
    }
}

impl Sb2Header {
    fn from_bytes(i: &[u8]) -> Result<Self> {
        let (remainder_len, header) = Self::inner_from_bytes(i)
            .map(|(remainder, header)| (remainder.len(), header))?;
        match remainder_len {
            0 => Ok(header),
            _ => Err(anyhow::anyhow!("spurious bytes")),
        }
    }

    fn inner_from_bytes(i: &[u8]) -> nom::IResult<&[u8], Self, ()> {
        // if i.len() != 0x60 {
        //     return Err(anyhow::anyhow!("wrong size for SB2 header"));
        // }
        let mut nonce = [0u32; 4];
        let (i, ()) = fill(le_u32, &mut nonce)(i)?;
        // let mut nonce = [0u8; 16];
        // let (i, ()) = fill(u8, &mut nonce)(i)?;

        let (i, _reserved) = take(4u8)(i)?;

        let (i, _signature) = tag("STMP")(i)?;

        // are these not somewhere in `nom` already??
        let literal_u8 = |x: u8| verify(u8, move |y| *y == x);
        let literal_u16 = |x: u16| verify(le_u16, move |y| *y == x);
        let literal_u32 = |x: u32| verify(le_u32, move |y| *y == x);

        // header_version_major should be 2u8
        let (i, _) = literal_u8(2u8)(i)?;
        // header_version_major should be 0u8 or 1u8
        let (i, header_version_minor) = alt((literal_u8(0), literal_u8(1)))(i)?;

        let (i, flags) = le_u16(i)?;
        let (i, image_size_blocks) = le_u32(i)?;
        let (i, boot_tag_offset_blocks) = le_u32(i)?;
        let (i, boot_section_id) = literal_u32(0)(i)?;
        let (i, certificate_block_header_offset_bytes) = le_u32(i)?;
        let (i, header_size_blocks) = literal_u16(6)(i)?;
        let (i, keyblob_offset_blocks) = literal_u16(8)(i)?;
        let (i, keyblob_size_blocks) = literal_u16(5)(i)?;
        let (i, max_section_mac_count) = literal_u16(1)(i)?;
        let (i, _signature2) = tag("sgtl")(i)?;
        let (i, timestamp_microseconds_since_millenium) = le_u64(i)?;
        let (i, product_version) = version(i)?;
        let (i, component_version) = version(i)?;
        let (i, build_number) = le_u32(i)?;
        let (i, _) = take(4u8)(i)?;
        // nom::exact!(i, take(4u8));

        Ok((i, Self {
            nonce,
            header_version_minor,
            flags,
            image_size_blocks,
            boot_tag_offset_blocks,
            boot_section_id,
            certificate_block_header_offset_bytes,
            header_size_blocks,
            keyblob_offset_blocks,
            keyblob_size_blocks,
            max_section_mac_count,
            timestamp_microseconds_since_millenium,
            product_version,
            component_version,
            build_number,
        }))
    }
}

//     struct sb2_header_t
//     {
//         uint32_t nonce[4];            //!< Nonce for AES-CTR
//         uint32_t reserved;            //!< Reserved, un-used
//         uint8_t m_signature[4];       //!< 'STMP', see #ROM_IMAGE_HEADER_SIGNATURE.
//         uint8_t m_majorVersion;       //!< Major version for the image format, see #ROM_BOOT_IMAGE_MAJOR_VERSION.
//         uint8_t m_minorVersion;       //!< Minor version of the boot image format, see #ROM_BOOT_IMAGE_MINOR_VERSION.
//         uint16_t m_flags;             //!< Flags or options associated with the entire image.
//         uint32_t m_imageBlocks;       //!< Size of entire image in blocks.
//         uint32_t m_firstBootTagBlock; //!< Offset from start of file to the first boot tag, in blocks.
//         section_id_t m_firstBootableSectionID; //!< ID of section to start booting from.
//         uint32_t m_offsetToCertificateBlockInBytes;     //! Offset in bytes to the certificate block header for a signed SB file.
//         uint16_t m_headerBlocks;               //!< Size of this header, including this size word, in blocks.
//         uint16_t m_keyBlobBlock;      //!< Block number where the key blob starts
//         uint16_t m_keyBlobBlockCount; //!< Number of cipher blocks occupied by the key blob.
//         uint16_t m_maxSectionMacCount; //!< Maximum number of HMAC table entries used in all sections of the SB file.
//         uint8_t m_signature2[4];      //!< Always set to 'sgtl'
//         uint64_t m_timestamp;         //!< Timestamp when image was generated in microseconds since 1-1-2000.
//         version_t m_productVersion;   //!< User controlled product version.
//         version_t m_componentVersion; //!< User controlled component version.
//         uint32_t m_buildNumber;          //!< User controlled build number.
//         uint8_t m_padding1[4];        //!< Padding to round up to next cipher block.
//     };

//fn assemble_sb_file(signed_image: &[u8], certificate_der: &[u8], rot_key_hashes: [[u8; 32]; 4], secret_key: &rsa::RSAPrivateKey) -> Vec<u8> {

//    let mut sb = Vec::new();

//    // ┌────────┬─────────────────────────┬─────────────────────────┬────────┬────────┐
//    // │00000000│ 6d d2 d1 ac 6b 14 82 cd ┊ 2f 03 95 2a cb 48 5b 06 │m×××k•××┊/•×*×H[•│
//    // │00000010│ 00 00 00 00 53 54 4d 50 ┊ 02 01 08 00 24 65 00 00 │0000STMP┊•••0$e00│
//    // │00000020│ 6e 00 00 00 00 00 00 00 ┊ d0 00 00 00 06 00 08 00 │n0000000┊×000•0•0│
//    // │00000030│ 05 00 01 00 73 67 74 6c ┊ 80 00 7e fe 4b 55 02 00 │•0•0sgtl┊×0~×KU•0│
//    // │00000040│ 00 00 00 00 00 00 00 00 ┊ 00 00 00 00 00 00 00 00 │00000000┊00000000│
//    // │00000050│ 00 00 00 00 00 00 00 00 ┊ 01 00 00 00 9f ab 65 6b │00000000┊•000××ek│

//    // ┌────────┬─────────────────────────┬─────────────────────────┬────────┬────────┐
//    // │00000000│ 9d ca ee 81 fe 74 84 bb ┊ 3d ec 71 03 94 5a 48 02 │×××××t××┊=×q•×ZH•│
//    // │00000010│ 00 00 00 00 53 54 4d 50 ┊ 02 01 08 00 26 65 00 00 │0000STMP┊•••0&e00│
//    // │00000020│ 6f 00 00 00 00 00 00 00 ┊ d0 00 00 00 06 00 08 00 │o0000000┊×000•0•0│
//    // │00000030│ 05 00 01 00 73 67 74 6c ┊ 80 2c a7 a9 98 58 02 00 │•0•0sgtl┊×,×××X•0│
//    // │00000040│ 00 00 00 00 00 00 00 00 ┊ 00 00 00 00 00 00 00 00 │00000000┊00000000│
//    // │00000050│ 00 00 00 00 00 00 00 00 ┊ 01 00 00 00 3b 91 93 65 │00000000┊•000;××e│

//    /////////////////////////////////
//    // First the header
//    // cf. SB2Image.h::sb2_header_t
//    /////////////////////////////////

//    // SB2Image.cpp:799 sets this to random, but clears bits 31 and 63 (whyy?)
//    sb.extend32(0x81eeca9d);
//    sb.extend32(0xbb8474fe);
//    sb.extend32(0x0371ec3d);
//    sb.extend32(0x02485a94);

//    // padding
//    sb.extend32(0)

//    // 'STMP' magic bytes aka "signature", comes from Freescale SigmaTel portable media player
//    // something something
//    sb.extend_from_slice(b"STMP");

//    // SB2.1 major = 2
//    sb.push(2)
//    // SB2.1 minor = 1
//    sb.push(1)
//    // m_flags: associated with entire image
//    todo!("figure out details");
//    sb.extend_from_slice(&[0x08, 0x00]);

//    // size of entire image in blocks
//    // NB: a block is an AES block = 16 bytes = 1 line in hexyl
//    // example we're reverse engineering (entire sb2 file!) is 414272B = 0x6524 * 16
//    sb.extend32(0x6526)

//    // first boot tag offset in blocks  (0x6e*16 would be 1776B)
//    sb.extend32(0x6f)
//    // ID of bootable section (there is only one, counting starts at zero (?confirm?))
//    sb.extend32(0)
//    // offset to certificate block in bytes
//    todo!("certificate block in bytes");
//    sb.extend32(0xd0)
//    // size of header in blocks
//    sb.extend16(0x0006);
//    // block number where keyblob starts: 8 --> 8*16 = 128 = 0x80
//    sb.extend16(0x0008);
//    // key blob block count
//    sb.extend16(0x0005);
//    // Maximum number of HMAC table entries used in all sections of the SB file.
//    sb.extend16(0x0001);
//    // 'sgtl' magic bytes aka "signature2" (this is only for SB2.1, so can "sniff" files to
//    // determine if they're SB2.1 via signature + signature2 (STMP/sgtl)
//    sb.extend_from_slice(b"sgtl");
//    // 64bit timestamp since jan 1, 2000
//    sb.extend64(0x00025898a9a72c08);

//    // version_t comes from Version.h, and is supposed to be big-endian BCD (u16 each for
//    // major/minor/patch, with one u16 padding each)
//    // product.major
//    sb.extend16(0);
//    sb.extend16(0);
//    // product.minor
//    sb.extend16(0);
//    sb.extend16(0);
//    // product.patch
//    sb.extend16(0);
//    sb.extend16(0);
//    // component.major
//    sb.extend16(0);
//    sb.extend16(0);
//    // component.minor
//    sb.extend16(0);
//    sb.extend16(0);
//    // component.patch
//    sb.extend16(0);
//    sb.extend16(0);

//    // build number
//    sb.extend32(1);
//    // padding to block size 16B
//    // they seem to want to use random numbers here (but why? the IV is already set randomly)
//    sb.extend32(0x6593913b);


//    // 0x50 - 0x80 = `digestHmac`
//    //
//    // │00000060│ 3b 0d 9f 42 3b ce 44 a7 ┊ c3 3f bc f2 fb f8 ac a5 │;_×B;×D×┊×?××××××│
//    // │00000070│ 95 0a 6d 3d db 84 0b 59 ┊ 0d 27 9c 84 d3 04 b0 75 │×_m=××•Y┊_'×××•×u│
//    todo!("figure out what this is an HMAC of");

//    // at 0x80, key blob starts, it is 5 blocks long (=5 lines)
//    // plaintext = DEK || MAC-key = 32B + 32B
//    // output of AES keywrap is: 64bit block + len(plaintext) = 8 + 64 = 72 = 4.5 blocks
//    // --> padded with zeros, this gives 5 blocks
//    //
//    // in this case (using the horribly python2 aes-keywrap (+pycrypto)
//    // DEK =     '1517ba1c12e23a210debf17037c33d1706653a53d2b7f4502d11012d6d0f8d8e'
//    // MAC key = '7917d9c678c7b018d9f817b4667773225fb210d39f10a2b54b18d91d1cd62285'
//    // │00000080│ 62 c3 83 2a 82 a7 7f 76 ┊ 1e cd 81 54 fc f7 60 84 │b××*××•v┊•××T××`×│
//    // │00000090│ ca 53 37 99 fb 27 b0 80 ┊ 6a 22 66 aa bc 5f 47 19 │×S7××'××┊j"f××_G•│
//    // │000000a0│ ed a9 c1 5a ec 8d 64 b6 ┊ b8 fd fc 61 f2 5e 04 d0 │×××Z××d×┊×××a×^•×│
//    // │000000b0│ 66 91 dc 00 3b 72 2f cb ┊ 3c 15 05 ff 7b d2 57 94 │f××0;r/×┊<••×{×W×│
//    // │000000c0│ 07 83 51 c5 79 91 7a a6 ┊ 00 00 00 00 00 00 00 00 │•×Q×y×z×┊00000000│

//    todo!("apply AES keywrap with 0xAAA..AAA to keys we like");

//    // at 0xd0, 'cert' signals beginning of certificate block header





//    todo!();
//}
//    // AN12283, section 2.3 "Signed image"
//    // First comes a header: elftosb/common/AuthImageGenerator.h:200-210 (es_header_t)
//    // - 0x0 ??
//    // - 0x20 image length                <-- this and following set in elftosb/common/AuthImageGenerator.cpp:1204
//    // - 0x24 image type: "SPT"
//    // - 0x28 header offset
//    // - ??
//    // - 0x34 load address
//    // - plain image (assume this means .bin?)
//    // - certificate block header
//    // - X.509 certificate
//    // - RoT key 0 hash
//    // - RoT key 1 hash
//    // - RoT key 2 hash
//    // - RoT key 3 hash
//    // - data (TrustZone config)
//    // - RSASSA-PKCS1-v1_5 signature
//    //

//    // Instructions:
//    // - load .bin file, pad to 4 bytes with zeros (-> image_size)
//    // - patch in 0x20 (image_size, u32-le), 0x24 ('04 40 00 00' = '<signed> <no-TZ> 00 00'), 0x28
//    // = uhhh.. double check the two numbers at 0x20 and 0x28
//    // - save padded+modified .bin file
//    // - add certificate header block:
//    //   'cert'    01      02     00
//    //   build  imglen certs=1  certslen = 4 + padded DER cert len
//    // - add cert, prefixed by its length (u32-le), padded with zeros to 4-byte alignment
//    // - add 4x ROT SHA2 (32B each)
//    // - add 256B RSASSA PKCS v1.5 signature

//    let length = bin.len();
//    assert!(length % 8 == 0);
//    modify_header(&mut bin);
//}