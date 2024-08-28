//! Secret keys for the BFV encryption scheme

use crate::bfv::{BfvParameters, Ciphertext, Plaintext};
use crate::{Error, Result};
use fhe_math::{
    rq::{traits::TryConvertFrom, Poly, Representation},
    zq::Modulus,
};
use fhe_traits::{DeserializeParametrized, FheDecrypter, FheEncrypter, FheParametrized, Serialize};
use fhe_util::sample_vec_cbd;
use itertools::Itertools;
use num_bigint::BigUint;
use rand::{Rng, RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
extern crate alloc;
use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use zeroize::Zeroizing;
use zeroize_derive::{Zeroize, ZeroizeOnDrop};

/// Secret key for the BFV encryption scheme.
#[derive(Debug, PartialEq, Eq, Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretKey {
    #[zeroize(skip)]
    pub(crate) par: Arc<BfvParameters>,
    pub(crate) coeffs: Box<[i64]>,
}

impl SecretKey {
    /// Generate a random [`SecretKey`].
    pub fn random<R: RngCore>(par: &Arc<BfvParameters>, rng: &mut R) -> Self {
        let s_coefficients = sample_vec_cbd(par.degree(), par.variance, rng).unwrap();
        Self::new(s_coefficients, par)
    }

    /// Generate a [`SecretKey`] from its coefficients.
    pub(crate) fn new(coeffs: Vec<i64>, par: &Arc<BfvParameters>) -> Self {
        Self {
            par: par.to_owned(),
            coeffs: coeffs.into_boxed_slice(),
        }
    }

    /// Measure the noise in a [`Ciphertext`].
    ///
    /// # Safety
    ///
    /// This operations may run in a variable time depending on the value of the
    /// noise.
    pub unsafe fn measure_noise(&self, ct: &Ciphertext) -> Result<usize> {
        let plaintext = Zeroizing::new(self.try_decrypt(ct)?);
        let m = Zeroizing::new(plaintext.to_poly());

        // Let's create a secret key with the ciphertext context
        let mut s = Zeroizing::new(Poly::try_convert_from(
            self.coeffs.as_ref(),
            ct[0].ctx(),
            false,
            Representation::PowerBasis,
        )?);
        s.change_representation(Representation::Ntt);
        let mut si = s.clone();

        // Let's disable variable time computations
        let mut c = Zeroizing::new(ct[0].clone());
        c.disallow_variable_time_computations();

        for i in 1..ct.len() {
            let mut cis = Zeroizing::new(ct[i].clone());
            cis.disallow_variable_time_computations();
            *cis.as_mut() *= si.as_ref();
            *c.as_mut() += &cis;
            *si.as_mut() *= s.as_ref();
        }
        *c.as_mut() -= &m;
        c.change_representation(Representation::PowerBasis);

        let ciphertext_modulus = ct[0].ctx().modulus();
        let mut noise = 0usize;
        for coeff in Vec::<BigUint>::from(c.as_ref()) {
            noise = core::cmp::max(
                noise,
                core::cmp::min(coeff.bits(), (ciphertext_modulus - &coeff).bits()) as usize,
            )
        }

        Ok(noise)
    }

    pub(crate) fn encrypt_poly<R: RngCore>(&self, p: &Poly, rng: &mut R) -> Result<Ciphertext> {
        assert_eq!(p.representation(), &Representation::Ntt);

        let level = self.par.level_of_ctx(p.ctx())?;

        let mut seed = <ChaCha8Rng as SeedableRng>::Seed::default();
        rng.fill(&mut seed);

        // Let's create a secret key with the ciphertext context
        let mut s = Zeroizing::new(Poly::try_convert_from(
            self.coeffs.as_ref(),
            p.ctx(),
            false,
            Representation::PowerBasis,
        )?);
        s.change_representation(Representation::Ntt);

        let mut a = Poly::random_from_seed(p.ctx(), Representation::Ntt, seed);
        let a_s = Zeroizing::new(&a * s.as_ref());

        let mut b = Poly::small(p.ctx(), Representation::Ntt, self.par.variance, rng)
            .map_err(Error::MathError)?;
        b -= &a_s;
        b += p;

        // It is now safe to enable variable time computations.
        unsafe {
            a.allow_variable_time_computations();
            b.allow_variable_time_computations()
        }

        Ok(Ciphertext {
            par: self.par.clone(),
            seed: Some(seed),
            c: vec![b, a],
            level,
        })
    }
}

impl FheParametrized for SecretKey {
    type Parameters = BfvParameters;
}

impl Serialize for SecretKey {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Serialize the coefficients
        let coeffs_len = self.coeffs.len() as u64; // Length of coeffs as u64
        bytes.extend_from_slice(&coeffs_len.to_le_bytes()); // Add the length
        for coeff in self.coeffs.iter() {
            bytes.extend_from_slice(&coeff.to_le_bytes());
        }

        bytes
    }
}

impl DeserializeParametrized for SecretKey {
    type Error = Error;

    fn from_bytes(bytes: &[u8], par: &Arc<Self::Parameters>) -> Result<Self> {
        let mut cursor = 0;

        // Ensure we have at least 8 bytes to read the coeffs_len
        if bytes.len() < cursor + 8 {
            return Err(Error::DefaultError(
                "Invalid byte length for SecretKey deserialization".to_string(),
            ));
        }

        // Deserialize the length of coeffs
        let coeffs_len = u64::from_le_bytes(bytes[cursor..cursor + 8].try_into().map_err(|_| {
            Error::DefaultError("Failed to convert bytes to coeffs_len".to_string())
        })?) as usize;
        cursor += 8;

        // Ensure that the remaining byte slice has enough data for all coefficients
        let required_len = coeffs_len.checked_mul(8).ok_or_else(|| {
            Error::DefaultError("Coefficient length multiplication overflow".to_string())
        })?;
        if bytes.len() < cursor + required_len {
            return Err(Error::DefaultError(
                "Invalid byte length for SecretKey deserialization".to_string(),
            ));
        }

        // Deserialize the coefficients
        let mut coeffs = Vec::with_capacity(coeffs_len);
        for _ in 0..coeffs_len {
            let coeff = i64::from_le_bytes(bytes[cursor..cursor + 8].try_into().map_err(|_| {
                Error::DefaultError("Failed to convert bytes to coefficient".to_string())
            })?);
            coeffs.push(coeff);
            cursor += 8;
        }

        // Return the deserialized SecretKey with the externally provided parameters
        Ok(Self {
            par: par.clone(),
            coeffs: coeffs.into_boxed_slice(),
        })
    }
}
impl FheEncrypter<Plaintext, Ciphertext> for SecretKey {
    type Error = Error;

    fn try_encrypt<R: RngCore>(&self, pt: &Plaintext, rng: &mut R) -> Result<Ciphertext> {
        assert_eq!(self.par, pt.par);
        let m = Zeroizing::new(pt.to_poly());
        self.encrypt_poly(m.as_ref(), rng)
    }
}

impl FheDecrypter<Plaintext, Ciphertext> for SecretKey {
    type Error = Error;

    fn try_decrypt(&self, ct: &Ciphertext) -> Result<Plaintext> {
        if self.par != ct.par {
            Err(Error::DefaultError(
                "Incompatible BFV parameters".to_string(),
            ))
        } else {
            // Let's create a secret key with the ciphertext context
            let mut s = Zeroizing::new(Poly::try_convert_from(
                self.coeffs.as_ref(),
                ct[0].ctx(),
                false,
                Representation::PowerBasis,
            )?);
            s.change_representation(Representation::Ntt);
            let mut si = s.clone();

            let mut c = Zeroizing::new(ct[0].clone());
            c.disallow_variable_time_computations();

            // Compute the phase c0 + c1*s + c2*s^2 + ... where the secret power
            // s^k is computed on-the-fly
            for i in 1..ct.len() {
                let mut cis = Zeroizing::new(ct[i].clone());
                cis.disallow_variable_time_computations();
                *cis.as_mut() *= si.as_ref();
                *c.as_mut() += &cis;
                if i + 1 < ct.len() {
                    *si.as_mut() *= s.as_ref();
                }
            }
            c.change_representation(Representation::PowerBasis);

            let d = Zeroizing::new(c.scale(&self.par.scalers[ct.level])?);

            // TODO: Can we handle plaintext moduli that are BigUint?
            let v = Zeroizing::new(
                Vec::<u64>::from(d.as_ref())
                    .iter_mut()
                    .map(|vi| *vi + *self.par.plaintext)
                    .collect_vec(),
            );
            let mut w = v[..self.par.degree()].to_vec();
            let q = Modulus::new(self.par.moduli[0]).map_err(Error::MathError)?;
            q.reduce_vec(&mut w);
            self.par.plaintext.reduce_vec(&mut w);

            let mut poly =
                Poly::try_convert_from(&w, ct[0].ctx(), false, Representation::PowerBasis)?;
            poly.change_representation(Representation::Ntt);

            let pt = Plaintext {
                par: self.par.clone(),
                value: w.into_boxed_slice(),
                encoding: None,
                poly_ntt: poly,
                level: ct.level,
            };

            Ok(pt)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SecretKey;
    use crate::bfv::{parameters::BfvParameters, Encoding, Plaintext};
    use crate::Error;
    use fhe_traits::{FheDecrypter, FheEncoder, FheEncrypter};
    use rand::thread_rng;

    #[test]
    fn keygen() {
        let mut rng = thread_rng();
        let params = BfvParameters::default_arc(1, 16);
        let sk = SecretKey::random(&params, &mut rng);
        assert_eq!(sk.par, params);

        sk.coeffs.iter().for_each(|ci| {
            // Check that this is a small polynomial
            assert!((*ci).abs() <= 2 * sk.par.variance as i64)
        })
    }

    #[test]
    fn encrypt_decrypt() -> Result<(), Error> {
        let mut rng = thread_rng();
        for params in [
            BfvParameters::default_arc(1, 16),
            BfvParameters::default_arc(6, 16),
        ] {
            for level in 0..params.max_level() {
                for _ in 0..20 {
                    let sk = SecretKey::random(&params, &mut rng);

                    let pt = Plaintext::try_encode(
                        &params.plaintext.random_vec(params.degree(), &mut rng),
                        Encoding::poly_at_level(level),
                        &params,
                    )?;
                    let ct = sk.try_encrypt(&pt, &mut rng)?;
                    let pt2 = sk.try_decrypt(&ct)?;

                    //println!("Noise: {}", unsafe { sk.measure_noise(&ct)? });
                    assert_eq!(pt2, pt);
                }
            }
        }

        Ok(())
    }
}