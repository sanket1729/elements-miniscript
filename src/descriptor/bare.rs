// Miniscript
// Written in 2020 by rust-miniscript developers
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! # Bare Output Descriptors
//!
//! Implementation of Bare Descriptors (i.e descriptors that are)
//! wrapped inside wsh, or sh fragments.
//! Also includes pk, and pkh descriptors
//!

use std::{fmt, str::FromStr};

use bitcoin::secp256k1;
use elements::{self, script, Script};

use expression::{self, FromTree};
use miniscript::context::ScriptContext;
use policy::{semantic, Liftable};
use util::{varint_len, witness_to_scriptsig};
use {BareCtx, Error, Miniscript, MiniscriptKey, Satisfier, ToPublicKey};

use super::{
    checksum::{desc_checksum, verify_checksum},
    DescriptorTrait, ElementsTrait, PkTranslate,
};

/// Create a Bare Descriptor. That is descriptor that is
/// not wrapped in sh or wsh. This covers the Pk descriptor
#[derive(Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct Bare<Pk: MiniscriptKey> {
    /// underlying miniscript
    ms: Miniscript<Pk, BareCtx>,
}

impl<Pk: MiniscriptKey> Bare<Pk> {
    /// Create a new raw descriptor
    pub fn new(ms: Miniscript<Pk, BareCtx>) -> Result<Self, Error> {
        // do the top-level checks
        BareCtx::top_level_checks(&ms)?;
        Ok(Self { ms: ms })
    }

    /// get the inner
    pub fn as_inner(&self) -> &Miniscript<Pk, BareCtx> {
        &self.ms
    }
}

impl<Pk: MiniscriptKey> fmt::Debug for Bare<Pk> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.ms)
    }
}

impl<Pk: MiniscriptKey> fmt::Display for Bare<Pk> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let desc = format!("{}", self.ms);
        let checksum = desc_checksum(&desc).map_err(|_| fmt::Error)?;
        write!(f, "{}#{}", &desc, &checksum)
    }
}

impl<Pk: MiniscriptKey> Liftable<Pk> for Bare<Pk> {
    fn lift(&self) -> Result<semantic::Policy<Pk>, Error> {
        self.ms.lift()
    }
}

impl<Pk: MiniscriptKey> FromTree for Bare<Pk>
where
    <Pk as FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as FromStr>::Err: ToString,
{
    fn from_tree(top: &expression::Tree) -> Result<Self, Error> {
        let sub = Miniscript::<Pk, BareCtx>::from_tree(&top)?;
        BareCtx::top_level_checks(&sub)?;
        Bare::new(sub)
    }
}

impl<Pk: MiniscriptKey> FromStr for Bare<Pk>
where
    <Pk as FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as FromStr>::Err: ToString,
{
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let desc_str = verify_checksum(s)?;
        let top = expression::Tree::from_str(desc_str)?;
        Self::from_tree(&top)
    }
}

impl<Pk: MiniscriptKey> ElementsTrait<Pk> for Bare<Pk> {
    fn blind_addr<ToPkCtx: Copy>(
        &self,
        _to_pk_ctx: ToPkCtx,
        _blinder: Option<secp256k1::PublicKey>,
        _params: &'static elements::AddressParams,
    ) -> Option<elements::Address>
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        None
    }
}
impl<Pk: MiniscriptKey> DescriptorTrait<Pk> for Bare<Pk>
where
    <Pk as FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as FromStr>::Err: ToString,
{
    fn sanity_check(&self) -> Result<(), Error> {
        self.ms.sanity_check()?;
        Ok(())
    }

    fn address<ToPkCtx: Copy>(
        &self,
        _to_pk_ctx: ToPkCtx,
        _network: &elements::AddressParams,
    ) -> Option<elements::Address>
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        None
    }

    fn script_pubkey<ToPkCtx: Copy>(&self, to_pk_ctx: ToPkCtx) -> Script
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        self.ms.encode(to_pk_ctx)
    }

    fn unsigned_script_sig<ToPkCtx: Copy>(&self, _to_pk_ctx: ToPkCtx) -> Script
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        Script::new()
    }

    fn witness_script<ToPkCtx: Copy>(&self, to_pk_ctx: ToPkCtx) -> Script
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        self.ms.encode(to_pk_ctx)
    }

    fn get_satisfaction<ToPkCtx, S>(
        &self,
        satisfier: S,
        to_pk_ctx: ToPkCtx,
    ) -> Result<(Vec<Vec<u8>>, Script), Error>
    where
        ToPkCtx: Copy,
        Pk: ToPublicKey<ToPkCtx>,
        S: Satisfier<ToPkCtx, Pk>,
    {
        let ms = self.ms.satisfy(satisfier, to_pk_ctx)?;
        let script_sig = witness_to_scriptsig(&ms);
        let witness = vec![];
        Ok((witness, script_sig))
    }

    fn max_satisfaction_weight(&self) -> Option<usize> {
        let scriptsig_len = self.ms.max_satisfaction_size()?;
        Some(4 * (varint_len(scriptsig_len) + scriptsig_len))
    }

    fn script_code<ToPkCtx: Copy>(&self, to_pk_ctx: ToPkCtx) -> Script
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        self.script_pubkey(to_pk_ctx)
    }
}

impl<P: MiniscriptKey, Q: MiniscriptKey> PkTranslate<P, Q> for Bare<P> {
    type Output = Bare<Q>;

    fn translate_pk<Fpk, Fpkh, E>(
        &self,
        mut translatefpk: Fpk,
        mut translatefpkh: Fpkh,
    ) -> Result<Self::Output, E>
    where
        Fpk: FnMut(&P) -> Result<Q, E>,
        Fpkh: FnMut(&P::Hash) -> Result<Q::Hash, E>,
        Q: MiniscriptKey,
    {
        Ok(Bare::new(
            self.ms
                .translate_pk(&mut translatefpk, &mut translatefpkh)?,
        )
        .expect("Translation cannot fail inside Bare"))
    }
}

/// A bare PkH descriptor at top level
#[derive(Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct Pkh<Pk: MiniscriptKey> {
    /// underlying publickey
    pk: Pk,
}

impl<Pk: MiniscriptKey> Pkh<Pk> {
    /// Create a new Pkh descriptor
    pub fn new(pk: Pk) -> Self {
        // do the top-level checks
        Self { pk: pk }
    }

    /// Get the inner key
    pub fn as_inner(&self) -> &Pk {
        &self.pk
    }
}

impl<Pk: MiniscriptKey> fmt::Debug for Pkh<Pk> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "pkh({:?})", self.pk)
    }
}

impl<Pk: MiniscriptKey> fmt::Display for Pkh<Pk> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let desc = format!("pkh({})", self.pk);
        let checksum = desc_checksum(&desc).map_err(|_| fmt::Error)?;
        write!(f, "{}#{}", &desc, &checksum)
    }
}

impl<Pk: MiniscriptKey> Liftable<Pk> for Pkh<Pk> {
    fn lift(&self) -> Result<semantic::Policy<Pk>, Error> {
        Ok(semantic::Policy::KeyHash(self.pk.to_pubkeyhash()))
    }
}

impl<Pk: MiniscriptKey> FromTree for Pkh<Pk>
where
    <Pk as FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as FromStr>::Err: ToString,
{
    fn from_tree(top: &expression::Tree) -> Result<Self, Error> {
        if top.name == "pkh" && top.args.len() == 1 {
            Ok(Pkh::new(expression::terminal(&top.args[0], |pk| {
                Pk::from_str(pk)
            })?))
        } else {
            Err(Error::Unexpected(format!(
                "{}({} args) while parsing pkh descriptor",
                top.name,
                top.args.len(),
            )))
        }
    }
}

impl<Pk: MiniscriptKey> FromStr for Pkh<Pk>
where
    <Pk as FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as FromStr>::Err: ToString,
{
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let desc_str = verify_checksum(s)?;
        let top = expression::Tree::from_str(desc_str)?;
        Self::from_tree(&top)
    }
}
impl<Pk: MiniscriptKey> ElementsTrait<Pk> for Pkh<Pk> {
    fn blind_addr<ToPkCtx: Copy>(
        &self,
        to_pk_ctx: ToPkCtx,
        blinder: Option<secp256k1::PublicKey>,
        params: &'static elements::AddressParams,
    ) -> Option<elements::Address>
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        Some(elements::Address::p2pkh(
            &self.pk.to_public_key(to_pk_ctx),
            blinder,
            params,
        ))
    }
}

impl<Pk: MiniscriptKey> DescriptorTrait<Pk> for Pkh<Pk>
where
    <Pk as FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as FromStr>::Err: ToString,
{
    fn sanity_check(&self) -> Result<(), Error> {
        Ok(())
    }

    fn address<ToPkCtx: Copy>(
        &self,
        to_pk_ctx: ToPkCtx,
        params: &'static elements::AddressParams,
    ) -> Option<elements::Address>
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        Some(elements::Address::p2pkh(
            &self.pk.to_public_key(to_pk_ctx),
            None,
            params,
        ))
    }

    fn script_pubkey<ToPkCtx: Copy>(&self, to_pk_ctx: ToPkCtx) -> Script
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        let addr = elements::Address::p2pkh(
            &self.pk.to_public_key(to_pk_ctx),
            None,
            &elements::AddressParams::ELEMENTS,
        );
        addr.script_pubkey()
    }

    fn unsigned_script_sig<ToPkCtx: Copy>(&self, _to_pk_ctx: ToPkCtx) -> Script
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        Script::new()
    }

    fn witness_script<ToPkCtx: Copy>(&self, to_pk_ctx: ToPkCtx) -> Script
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        self.script_pubkey(to_pk_ctx)
    }

    fn get_satisfaction<ToPkCtx, S>(
        &self,
        satisfier: S,
        to_pk_ctx: ToPkCtx,
    ) -> Result<(Vec<Vec<u8>>, Script), Error>
    where
        ToPkCtx: Copy,
        Pk: ToPublicKey<ToPkCtx>,
        S: Satisfier<ToPkCtx, Pk>,
    {
        if let Some(sig) = satisfier.lookup_sig(&self.pk, to_pk_ctx) {
            let mut sig_vec = sig.0.serialize_der().to_vec();
            sig_vec.push(sig.1.as_u32() as u8);
            let script_sig = script::Builder::new()
                .push_slice(&sig_vec[..])
                .push_key(&self.pk.to_public_key(to_pk_ctx))
                .into_script();
            let witness = vec![];
            Ok((witness, script_sig))
        } else {
            Err(Error::MissingSig(self.pk.to_public_key(to_pk_ctx)))
        }
    }

    fn max_satisfaction_weight(&self) -> Option<usize> {
        Some(4 * (1 + 73 + self.pk.serialized_len()))
    }

    fn script_code<ToPkCtx: Copy>(&self, to_pk_ctx: ToPkCtx) -> Script
    where
        Pk: ToPublicKey<ToPkCtx>,
    {
        self.script_pubkey(to_pk_ctx)
    }
}

impl<P: MiniscriptKey, Q: MiniscriptKey> PkTranslate<P, Q> for Pkh<P> {
    type Output = Pkh<Q>;

    fn translate_pk<Fpk, Fpkh, E>(
        &self,
        mut translatefpk: Fpk,
        _translatefpkh: Fpkh,
    ) -> Result<Self::Output, E>
    where
        Fpk: FnMut(&P) -> Result<Q, E>,
        Fpkh: FnMut(&P::Hash) -> Result<Q::Hash, E>,
        Q: MiniscriptKey,
    {
        Ok(Pkh::new(translatefpk(&self.pk)?))
    }
}
