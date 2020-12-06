use crate::error::Result;
use crate::types::to_hex_string;
use crate::pfr::{FieldAreaPage, Keystore, FactoryArea, Sha256Hash};

use core::convert::TryInto;
use std::fs;

use rsa::PublicKeyParts as _;
use sha2::Digest as _;
use serde::{Deserialize, Serialize};

// #[derive(Clone, Debug, Deserialize, Serialize)]
// pub struct CfpaConfig {
//     pub rot_keys_status: RotKeysStatus,
//     pub boot_configuration: BootConfiguration,
//     pub usb_vid_pid: UsbVidPid,
// }

// #[derive(Clone, Debug, Deserialize, Serialize)]
// pub struct CmpaConfig {
//     pub secure_boot_configuration: SecureBootConfiguration,
// }

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub root_cert_filenames: [String; 4],
    pub factory: FactoryArea,
    pub field: FieldAreaPage,
    pub keystore: Keystore,
}

use x509_parser::der_parser::oid;

pub fn rot_key_hashes(certs: &[String; 4]) -> Result<[[u8; 32]; 4]> {
    let mut hashes = [[0u8; 32]; 4];
    for (i, cert_filename) in certs.iter().enumerate() {
        let cert_content = fs::read(cert_filename)?;
        let (rem, cert) = x509_parser::parse_x509_der(&cert_content)?;
        assert!(rem.is_empty());

        let spki = cert.tbs_certificate.subject_pki;
        trace!("alg: {:?}", spki.algorithm.algorithm);
        // let OID_RSA_ENCRYPTION = oid!(1.2.840.113549.1.1.1);
        assert_eq!(oid_registry::OID_PKCS1_RSAENCRYPTION, spki.algorithm.algorithm);

        let public_key = rsa::RSAPublicKey::from_pkcs1(&spki.subject_public_key.data)?;
        let n = public_key.n();
        let e = public_key.e();
        // e.g., n = 21180609610011908974245154634009773742409228475924832420640732487602371552607208434815604239733761061624595772266076892772797402260546921881940097799828803122149358818132191889899441450923166919457193292916001584543268399036684342230632304039418343776750540042195439119799724089028829483927297432554313701904867373619640752457487405782173272827509578742485272792121363761115153135595006648746766049001063218844454972346390444289285459567420247245376227517357296502996294373645061373559719690903237831034883266667892726893796797389886027843919406367649873994790265470728806388647429250289865772615066974316813540762961
        trace!("n = {}, e = {}", n, e);
        debug!("n bytes = \"{}\"", hex_str!(&n.to_bytes_be(), 4));

        let mut hasher = sha2::Sha256::new();
        hasher.update(n.to_bytes_be());
        hasher.update(e.to_bytes_be());
        let result = hasher.finalize();
        hashes[i].copy_from_slice(&result);
    }
    Ok(hashes)
}

pub fn calculate(config_filename: &str) -> Result<()> {
    let config = fs::read_to_string(config_filename)?;
    let mut config: Config = toml::from_str(&config)?;
    // config.factory.prince_subregions[2] = crate::pfr::PrinceSubregion::from_bits_truncate(0x55);
    // debug!("loaded config:\n\n{}", serde_yaml::to_string(&config)?);
    // debug!("loaded config:\n\n{}", toml::to_string(&config)?);
    // debug!("loaded config:\n\n{:?}", &config);

    let mut hash = sha2::Sha256::new();

    for cert_filename in config.root_cert_filenames.iter() {
        let cert_content = fs::read(cert_filename)?;
        let (rem, cert) = x509_parser::parse_x509_der(&cert_content)?;
        assert!(rem.is_empty());
        let spki = cert.tbs_certificate.subject_pki;
        trace!("alg: {:?}", spki.algorithm.algorithm);
        assert_eq!(oid_registry::OID_PKCS1_RSAENCRYPTION, spki.algorithm.algorithm);

        let public_key = rsa::RSAPublicKey::from_pkcs1(&spki.subject_public_key.data)?;
        let n = public_key.n();
        let e = public_key.e();
        trace!("n = {}, e = {}", n, e);
        debug!("hex n = \n{}", hex_str!(&n.to_bytes_be(), 32, sep: "\n"));
        debug!("n bytes = \"{}\"", hex_str!(&n.to_bytes_be(), 4));

        let mut hasher = sha2::Sha256::new();
        hasher.update(n.to_bytes_be());
        hasher.update(e.to_bytes_be());
        let result = hasher.finalize();

        hash.update(result);
    }

    let rotkh = hash.finalize();
    config.factory.rot_keys_table_hash = Sha256Hash(rotkh.try_into().unwrap());
    info!("rotkh = {}", to_hex_string(&rotkh));
    println!("{}", to_hex_string(&rotkh));

    debug!("loaded config: {}", serde_yaml::to_string(&config)?);
    debug!("rot_keys_status as u32: 0x{:x}", u32::from(config.field.rot_keys_status));
    debug!("boot_configuration as u32: 0x{:x}", u32::from(config.factory.boot_configuration));
    debug!("secure_boot_configuration as u32: 0x{:x}", u32::from(config.factory.secure_boot_configuration));

    debug!("factory: {}", to_hex_string(config.factory.to_bytes().as_ref()));
    debug!("field: {}", to_hex_string(config.field.to_bytes().as_ref()));
    debug!("keystore: {}", to_hex_string(config.keystore.to_bytes().as_ref()));

    Ok(())
}
