//! Implementation for Tor certificates
//!
//! # Overview
//!
//! The `tor-cert` crate implements the binary certificate types
//! documented in Tor's cert-spec.txt, which are used when
//! authenticating Tor channels.  (Eventually, support for onion service
//! certificate support will get added too.)
//!
//! This crate is part of
//! [Arti](https://gitlab.torproject.org/tpo/core/arti/), a project to
//! implement [Tor](https://www.torproject.org/) in Rust.
//!
//! There are other types of certificate used by Tor as well, and they
//! are implemented in other places.  In particular, see
//! [`tor-netdoc::doc::authcert`] for the certificate types used by
//! authorities in the directory protocol.
//!
//! ## Design notes
//!
//! The `tor-cert` code is in its own separate crate because it is
//! required by several other higher-level crates that do not depend
//! upon each other.  For example, [`tor-netdoc`] parses encoded
//! certificates from router descriptors, while [`tor-proto`] uses
//! certificates when authenticating relays.
//!
//! # Examples
//!
//! Parsing, validating, and inspecting a certificate:
//!
//! ```
//! use base64::decode;
//! use tor_cert::*;
//! use tor_checkable::*;
//! // Taken from a random relay on the Tor network.
//! let cert_base64 =
//!  "AQQABrntAThPWJ4nFH1L77Ar+emd4GPXZTPUYzIwmR2H6Zod5TvXAQAgBAC+vzqh
//!   VFO1SGATubxcrZzrsNr+8hrsdZtyGg/Dde/TqaY1FNbeMqtAPMziWOd6txzShER4
//!   qc/haDk5V45Qfk6kjcKw+k7cPwyJeu+UF/azdoqcszHRnUHRXpiPzudPoA4=";
//! // Remove the whitespace, so base64 doesn't choke on it.
//! let cert_base64: String = cert_base64.split_whitespace().collect();
//! // Decode the base64.
//! let cert_bin = base64::decode(cert_base64).unwrap();
//!
//! // Decode the cert and check its signature.
//! let cert = Ed25519Cert::decode(&cert_bin).unwrap()
//!     .check_key(&None).unwrap()
//!     .check_signature().unwrap()
//!     .dangerously_assume_timely();
//! let signed_key = cert.subject_key();
//! ```

#![deny(missing_docs)]
#![warn(noop_method_call)]
#![deny(unreachable_pub)]
#![deny(clippy::await_holding_lock)]
#![deny(clippy::cargo_common_metadata)]
#![warn(clippy::clone_on_ref_ptr)]
#![warn(clippy::cognitive_complexity)]
#![deny(clippy::debug_assert_with_mut_call)]
#![deny(clippy::exhaustive_enums)]
#![deny(clippy::exhaustive_structs)]
#![deny(clippy::expl_impl_clone_on_copy)]
#![deny(clippy::fallible_impl_from)]
#![deny(clippy::large_stack_arrays)]
#![warn(clippy::manual_ok_or)]
#![deny(clippy::missing_docs_in_private_items)]
#![warn(clippy::option_option)]
#![warn(clippy::rc_buffer)]
#![deny(clippy::ref_option_ref)]
#![warn(clippy::trait_duplication_in_bounds)]
#![warn(clippy::unseparated_literal_suffix)]

pub mod rsa;

use caret::caret_int;
use signature::Verifier;
use tor_bytes::{Error, Result};
use tor_bytes::{Readable, Reader};
use tor_llcrypto::pk::*;

use std::time;

caret_int! {
    /// Recognized values for Tor's certificate type field.
    ///
    /// In the names used here, "X_V_Y" means "key X verifying key Y",
    /// whereas "X_CC_Y" means "key X cros-certifying key Y".  In both
    /// cases, X is the key that is doing the signing, and Y is the key
    /// or object that is getting signed.
    ///
    /// Not every one of these types is valid for an Ed25519
    /// certificate.  Some are for X.509 certs in a CERTS cell; some
    /// are for RSA->Ed crosscerts in a CERTS cell.
    pub struct CertType(u8) {
        /// TLS link key, signed with RSA identity. X.509 format. (Obsolete)
        TLS_LINK_X509 = 0x01,
        /// Self-signed RSA identity certificate. X.509 format. (Legacy)
        RSA_ID_X509 = 0x02,
        /// RSA lnk authentication key signed with RSA identity
        /// key. X.509 format. (Obsolete)
        LINK_AUTH_X509 = 0x03,

        /// Identity verifying a signing key, directly.
        IDENTITY_V_SIGNING = 0x04,

        /// Signing key verifying a TLS certificate by digest.
        SIGNING_V_TLS_CERT = 0x05,

        /// Signing key verifying a link authentication key.
        SIGNING_V_LINK_AUTH = 0x06,

        /// RSA identity key certifying an Ed25519 identity key. RSA
        /// crosscert format. (Legacy)
        RSA_ID_V_IDENTITY = 0x07,

        /// For onion services: short-term signing key authenticated with
        /// blinded service identity.
        HS_BLINDED_ID_V_SIGNING = 0x08,

        /// For onion services: to be documented.
        HS_IP_V_SIGNING = 0x09,

        /// An ntor key converted to a ed25519 key, cross-certifying an
        /// identity key.
        NTOR_CC_IDENTITY = 0x0A,

        /// For onion services: to be documented.
        HS_IP_CC_SIGNING = 0x0B,
    }
}

caret_int! {
    /// Extension identifiers for extensions in certificates.
    pub struct ExtType(u8) {
        /// Extension indicating an Ed25519 key that signed this certificate.
        ///
        /// Certificates do not always contain the key that signed them.
        SIGNED_WITH_ED25519_KEY = 0x04,
    }
}

caret_int! {
    /// Identifiers for the type of key or object getting signed.
    pub struct KeyType(u8) {
        /// Identifier for an Ed25519 key.
        ED25519_KEY = 0x01,
        /// Identifier for the SHA256 of an DER-encoded RSA key.
        SHA256_OF_RSA = 0x02,
        /// Identifies the SHA256 of an X.509 certificate.
        SHA256_OF_X509 = 0x03,

        // 08 through 09 and 0B are used for onion services.  They
        // probably shouldn't be, but that's what Tor does.
    }
}

/// Structure for an Ed25519-signed certificate as described in Tor's
/// cert-spec.txt.
pub struct Ed25519Cert {
    /// How many _hours_ after the epoch will this certificate expire?
    exp_hours: u32,
    /// Type of the certificate; recognized values are in certtype::*
    cert_type: CertType,
    /// The key or object being certified.
    cert_key: CertifiedKey,
    /// A list of extensions.
    #[allow(unused)]
    extensions: Vec<CertExt>,
    /// The key that signed this cert.
    ///
    /// Once the cert has been unwrapped from an KeyUnknownCert, this
    /// field will be set.
    signed_with: Option<ed25519::PublicKey>,
}

/// One of the data types that can be certified by an Ed25519Cert.
#[non_exhaustive]
pub enum CertifiedKey {
    /// An Ed25519 public key, signed directly.
    Ed25519(ed25519::PublicKey),
    /// The SHA256 digest of a DER-encoded RsaPublicKey
    RsaSha256Digest([u8; 32]),
    /// The SHA256 digest of an X.509 certificate.
    X509Sha256Digest([u8; 32]),
    /// Some unrecognized key type.
    Unrecognized(UnrecognizedKey),
}

/// A key whose type we didn't recognize.
pub struct UnrecognizedKey {
    /// Actual type of the key.
    key_type: KeyType,
    /// digest of the key, or the key itself.
    key_digest: [u8; 32],
}

impl CertifiedKey {
    /// Return the byte that identifies the type of this key.
    pub fn key_type(&self) -> KeyType {
        match self {
            CertifiedKey::Ed25519(_) => KeyType::ED25519_KEY,
            CertifiedKey::RsaSha256Digest(_) => KeyType::SHA256_OF_RSA,
            CertifiedKey::X509Sha256Digest(_) => KeyType::SHA256_OF_X509,

            CertifiedKey::Unrecognized(u) => u.key_type,
        }
    }
    /// Return the bytes that are used for the body of this certified
    /// key or object.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            CertifiedKey::Ed25519(k) => k.as_bytes(),
            CertifiedKey::RsaSha256Digest(k) => &k[..],
            CertifiedKey::X509Sha256Digest(k) => &k[..],
            CertifiedKey::Unrecognized(u) => &u.key_digest[..],
        }
    }
    /// If this is an Ed25519 public key, return Some(key).
    /// Otherwise, return None.
    pub fn as_ed25519(&self) -> Option<&ed25519::PublicKey> {
        match self {
            CertifiedKey::Ed25519(k) => Some(&k),
            _ => None,
        }
    }
    /// Try to extract a CertifiedKey from a Reader, given that we have
    /// already read its type as `key_type`.
    fn from_reader(key_type: KeyType, r: &mut Reader<'_>) -> Result<Self> {
        Ok(match key_type {
            KeyType::ED25519_KEY => CertifiedKey::Ed25519(r.extract()?),
            KeyType::SHA256_OF_RSA => CertifiedKey::RsaSha256Digest(r.extract()?),
            KeyType::SHA256_OF_X509 => CertifiedKey::X509Sha256Digest(r.extract()?),
            _ => CertifiedKey::Unrecognized(UnrecognizedKey {
                key_type,
                key_digest: r.extract()?,
            }),
        })
    }
}

/// An extension in a Tor certificate.
enum CertExt {
    /// Indicates which Ed25519 public key signed this cert.
    SignedWithEd25519(SignedWithEd25519Ext),
    /// An extension whose identity we don't recognize.
    Unrecognized(UnrecognizedExt),
}

/// Any unrecongized extension on a Tor certificate.
#[allow(unused)]
struct UnrecognizedExt {
    /// True iff this extension must be understand in order to validate the
    /// certificate.
    affects_validation: bool,
    /// The type of the extension
    ext_type: ExtType,
    /// The body of the extension.
    body: Vec<u8>,
}

impl CertExt {
    /// Return the identifier code for this Extension.
    fn ext_id(&self) -> ExtType {
        match self {
            CertExt::SignedWithEd25519(_) => ExtType::SIGNED_WITH_ED25519_KEY,
            CertExt::Unrecognized(u) => u.ext_type,
        }
    }
}

/*
impl Writeable for CertExt {
    fn write_onto<B: Writer + ?Sized>(&self, w: &mut B) {
        match self {
            CertExt::SignedWithEd25519(pk) => pk.write_onto(w),
            CertExt::Unrecognized(u) => u.write_onto(w),
        }
    }
}
 */

/// Extension indicating that a key that signed a given certificate.
struct SignedWithEd25519Ext {
    /// The key that signed the certificate including this extension.
    pk: ed25519::PublicKey,
}

/*
impl Writeable for SignedWithEd25519Ext {
    fn write_onto<B: Writer + ?Sized>(&self, w: &mut B) {
        // body length
        w.write_u16(32);
        // Signed-with-ed25519-key-extension
        w.write_u8(ExtType::SIGNED_WITH_ED25519_KEY.into());
        // flags = 0.
        w.write_u8(0);
        // body
        w.write_all(self.pk.as_bytes());
    }
}
*/

/*
impl UnrecognizedExt {
    /// Assert that there is no problem with the internal representation
    /// of this object.
    fn assert_rep_ok(&self) {
        assert!(self.body.len() <= std::u16::MAX as usize);
    }
}
*/

/*
impl Writeable for UnrecognizedExt {
    fn write_onto<B: Writer + ?Sized>(&self, w: &mut B) {
        self.assert_rep_ok();
        w.write_u16(self.body.len() as u16);
        w.write_u8(self.ext_type.into());
        let flags = if self.affects_validation { 1 } else { 0 };
        w.write_u8(flags);
        w.write_all(&self.body[..]);
    }
}
*/

impl Readable for CertExt {
    fn take_from(b: &mut Reader<'_>) -> Result<Self> {
        let len = b.take_u16()?;
        let ext_type: ExtType = b.take_u8()?.into();
        let flags = b.take_u8()?;
        let body = b.take(len as usize)?;

        Ok(match ext_type {
            ExtType::SIGNED_WITH_ED25519_KEY => {
                if body.len() != 32 {
                    return Err(Error::BadMessage("wrong length on Ed25519 key"));
                }
                CertExt::SignedWithEd25519(SignedWithEd25519Ext {
                    pk: ed25519::PublicKey::from_bytes(body)
                        .map_err(|_| Error::BadMessage("invalid Ed25519 public key"))?,
                })
            }
            _ => {
                if (flags & 1) != 0 {
                    return Err(Error::BadMessage(
                        "unrecognized certificate extension, with 'affects_validation' flag set.",
                    ));
                }
                CertExt::Unrecognized(UnrecognizedExt {
                    affects_validation: false,
                    ext_type,
                    body: body.into(),
                })
            }
        })
    }
}

impl Ed25519Cert {
    /*
        /// Helper: Assert that there is nothing wrong with the
        /// internal structure of this certificate.
        fn assert_rep_ok(&self) {
            assert!(self.extensions.len() <= std::u8::MAX as usize);
        }

        /// Encode a certificate into a new vector, signing the result
        /// with `keypair`.
        pub fn encode_and_sign(&self, skey: &ed25519::Keypair) -> Vec<u8> {
            self.assert_rep_ok();
            let mut w = Vec::new();
            w.write_u8(1); // Version
            w.write_u8(self.cert_type.into());
            w.write_u32(self.exp_hours);
            w.write_u8(self.cert_key.key_type().into());
            w.write_all(self.cert_key.as_bytes());

            for e in self.extensions.iter() {
                w.write(e);
            }

            let signature = skey.sign(&w[..]);
            w.write(&signature);
            w
        }
    */

    /// Try to decode a certificate from a byte slice.
    ///
    /// This function returns an error if the byte slice is not
    /// completely exhausted.
    ///
    /// Note that the resulting KeyUnknownCertificate is not checked
    /// for validity at all: you will need to provide it with an expected
    /// signing key, then check it for timeliness and well-signedness.
    pub fn decode(cert: &[u8]) -> Result<KeyUnknownCert> {
        let mut r = Reader::from_slice(cert);
        let v = r.take_u8()?;
        if v != 1 {
            // This would be something other than a "v1" certificate. We don't
            // understand those.
            return Err(Error::BadMessage("Unrecognized certificate version"));
        }
        let cert_type = r.take_u8()?.into();
        let exp_hours = r.take_u32()?;
        let mut cert_key_type = r.take_u8()?.into();

        // This is a workaround for a tor bug: the key type is
        // wrong. It was fixed in tor#40124, which got merged into Tor
        // 0.4.5.x and later.
        if cert_type == CertType::SIGNING_V_TLS_CERT && cert_key_type == KeyType::ED25519_KEY {
            cert_key_type = KeyType::SHA256_OF_X509;
        }

        let cert_key = CertifiedKey::from_reader(cert_key_type, &mut r)?;
        let n_exts = r.take_u8()?;
        let mut extensions = Vec::new();
        for _ in 0..n_exts {
            let e: CertExt = r.extract()?;
            extensions.push(e);
        }

        let sig_offset = r.consumed();
        let signature: ed25519::Signature = r.extract()?;
        r.should_be_exhausted()?;

        let keyext = extensions
            .iter()
            .find(|e| e.ext_id() == ExtType::SIGNED_WITH_ED25519_KEY);

        let included_pkey = match keyext {
            Some(CertExt::SignedWithEd25519(s)) => Some(s.pk),
            _ => None,
        };

        Ok(KeyUnknownCert {
            cert: UncheckedCert {
                cert: Ed25519Cert {
                    exp_hours,
                    cert_type,
                    cert_key,
                    extensions,

                    signed_with: included_pkey,
                },
                text: cert[0..sig_offset].into(),
                signature,
            },
        })
    }

    /// Return the time at which this certificate becomes expired
    pub fn expiry(&self) -> std::time::SystemTime {
        let d = std::time::Duration::new((self.exp_hours as u64) * 3600, 0);
        std::time::SystemTime::UNIX_EPOCH + d
    }

    /// Return true iff this certificate will be expired at the time `when`.
    pub fn is_expired_at(&self, when: std::time::SystemTime) -> bool {
        when >= self.expiry()
    }

    /// Return the signed key or object that is authenticated by this
    /// certificate.
    pub fn subject_key(&self) -> &CertifiedKey {
        &self.cert_key
    }

    /// Return the ed25519 key that signed this certificate.
    pub fn signing_key(&self) -> Option<&ed25519::PublicKey> {
        self.signed_with.as_ref()
    }

    /// Return the type of this certificate.
    pub fn cert_type(&self) -> CertType {
        self.cert_type
    }
}

/// A parsed Ed25519 certificate. Maybe it includes its signing key;
/// maybe it doesn't.
pub struct KeyUnknownCert {
    /// The certificate whose signing key might not be known.
    cert: UncheckedCert,
}

impl KeyUnknownCert {
    /// Return the certificate type of the underling cert.
    pub fn peek_cert_type(&self) -> CertType {
        self.cert.cert.cert_type
    }
    /// Return subject key of the underlying cert.
    pub fn peek_subject_key(&self) -> &CertifiedKey {
        &self.cert.cert.cert_key
    }

    /// Check whether a given pkey is (or might be) a key that has correctly
    /// signed this certificate.
    ///
    /// On success, we can check whether the certificate is well-signed;
    /// otherwise, we can't check the certificate.
    pub fn check_key(self, pkey: &Option<ed25519::PublicKey>) -> Result<UncheckedCert> {
        let real_key = match (pkey, self.cert.cert.signed_with) {
            (Some(a), Some(b)) if a == &b => b,
            (Some(_), Some(_)) => return Err(Error::BadMessage("Mismatched public key on cert")),
            (Some(a), None) => *a,
            (None, Some(b)) => b,
            (None, None) => return Err(Error::BadMessage("Missing public key on cert")),
        };
        Ok(UncheckedCert {
            cert: Ed25519Cert {
                signed_with: Some(real_key),
                ..self.cert.cert
            },
            ..self.cert
        })
    }
}

/// A certificate that has been parsed, but whose signature and
/// timeliness have not been checked.
pub struct UncheckedCert {
    /// The parsed certificate, possibly modified by inserting an externally
    /// supplied key as its signing key.
    cert: Ed25519Cert,

    /// The signed text of the certificate. (Checking ed25519 signatures
    /// forces us to store this.
    text: Vec<u8>, // XXXX It would be better to store a hash here.

    /// The alleged signature
    signature: ed25519::Signature,
}

/// A certificate that has been parsed and signature-checked, but whose
/// timeliness has not been checked.
pub struct SigCheckedCert {
    /// The certificate that might or might not be timely
    cert: Ed25519Cert,
}

impl UncheckedCert {
    /// Split this unchecked cert into a component that assumes it has
    /// been checked, and a signature to validate.
    pub fn dangerously_split(
        self,
    ) -> Result<(SigCheckedCert, ed25519::ValidatableEd25519Signature)> {
        use tor_checkable::SelfSigned;
        let signing_key = self
            .cert
            .signed_with
            .ok_or(Error::BadMessage("Missing public key on cert"))?;
        let signature =
            ed25519::ValidatableEd25519Signature::new(signing_key, self.signature, &self.text[..]);
        Ok((self.dangerously_assume_wellsigned(), signature))
    }

    /// Return subject key of the underlying cert.
    pub fn peek_subject_key(&self) -> &CertifiedKey {
        &self.cert.cert_key
    }
    /// Return signing key of the underlying cert.
    pub fn peek_signing_key(&self) -> &ed25519::PublicKey {
        self.cert
            .signed_with
            .as_ref()
            .expect("Made an UncheckedCert without a signing key")
    }
}

impl tor_checkable::SelfSigned<SigCheckedCert> for UncheckedCert {
    type Error = Error;

    fn is_well_signed(&self) -> Result<()> {
        let pubkey = &self.cert.signed_with.unwrap();
        pubkey
            .verify(&self.text[..], &self.signature)
            .map_err(|_| Error::BadMessage("Invalid certificate signature"))?;

        Ok(())
    }

    fn dangerously_assume_wellsigned(self) -> SigCheckedCert {
        SigCheckedCert { cert: self.cert }
    }
}

impl tor_checkable::Timebound<Ed25519Cert> for SigCheckedCert {
    type Error = tor_checkable::TimeValidityError;
    fn is_valid_at(&self, t: &time::SystemTime) -> std::result::Result<(), Self::Error> {
        if self.cert.is_expired_at(*t) {
            let expiry = self.cert.expiry();
            Err(Self::Error::Expired(
                t.duration_since(expiry)
                    .expect("certificate expiry time inconsistent"),
            ))
        } else {
            Ok(())
        }
    }

    fn dangerously_assume_timely(self) -> Ed25519Cert {
        self.cert
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use hex_literal::hex;

    #[test]
    fn parse_unrecognized_ext() -> Result<()> {
        // case one: a flag is set but we don't know it
        let b = hex!("0009 99 10 657874656e73696f6e");
        let mut r = Reader::from_slice(&b);
        let e: CertExt = r.extract()?;
        r.should_be_exhausted()?;

        assert_eq!(e.ext_id(), 0x99.into());

        // case two: we've been told to ignore the cert if we can't
        // handle the extension.
        let b = hex!("0009 99 11 657874656e73696f6e");
        let mut r = Reader::from_slice(&b);
        let e: Result<CertExt> = r.extract();
        assert!(e.is_err());
        assert_eq!(
            e.err().unwrap(),
            Error::BadMessage(
                "unrecognized certificate extension, with 'affects_validation' flag set."
            )
        );

        Ok(())
    }

    #[test]
    fn certified_key() -> Result<()> {
        let b =
            hex!("4c27616d6f757220756e6974206365757820717527656e636861c3ae6e616974206c6520666572");
        let mut r = Reader::from_slice(&b);

        let ck = CertifiedKey::from_reader(KeyType::SHA256_OF_RSA, &mut r)?;
        assert_eq!(ck.as_bytes(), &b[..32]);
        assert_eq!(ck.key_type(), KeyType::SHA256_OF_RSA);
        assert_eq!(r.remaining(), 7);

        let mut r = Reader::from_slice(&b);
        let ck = CertifiedKey::from_reader(42.into(), &mut r)?;
        assert_eq!(ck.as_bytes(), &b[..32]);
        assert_eq!(ck.key_type(), 42.into());
        assert_eq!(r.remaining(), 7);

        Ok(())
    }
}
