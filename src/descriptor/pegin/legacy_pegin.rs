// Miniscript
// Written in 2018 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
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

//! Pegin Descriptor Support
//!
//! Traits and implementations for Pegin descriptors
//! Note that this is a bitcoin descriptor and thus cannot be
//! added to elements Descriptor. Upstream rust-miniscript does
//! has a Descriptor enum which should ideally have it, but
//! bitcoin descriptors cannot depend on elements descriptors
//! Thus, as a simple solution we implement these as a separate
//! struct with it's own API.

use bitcoin::hashes::Hash;
use bitcoin::Script as BtcScript;
use bitcoin::{self, blockdata::script, hashes};
#[allow(deprecated)]
use bitcoin::{blockdata::opcodes, util::contracthash};
use bitcoin::{hashes::hash160, Address as BtcAddress};
use elements::secp256k1_zkp;
use expression::{self, FromTree};
use policy::{semantic, Liftable};
use std::{
    fmt::Debug,
    fmt::{self, Display},
    marker::PhantomData,
    str::FromStr,
    sync::Arc,
};
use Descriptor;
use Error;
use Miniscript;
use {
    BtcDescriptor, BtcDescriptorTrait, BtcError, BtcFromTree, BtcLiftable, BtcMiniscript,
    BtcPolicy, BtcSatisfier, BtcSegwitv0, BtcTerminal, BtcTree,
};

use {DescriptorTrait, Segwitv0, TranslatePk};

use {tweak_key, util::varint_len};

use descriptor::checksum::{desc_checksum, verify_checksum};

use super::PeginTrait;
use {MiniscriptKey, ToPublicKey};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// MiniscriptKey used for Pegins
pub enum LegacyPeginKey {
    /// Functionary Key that can be tweaked
    Functionary(bitcoin::PublicKey),
    /// Non functionary Key, cannot be tweaked
    NonFunctionary(bitcoin::PublicKey),
}

impl LegacyPeginKey {
    /// Get the untweaked version of the LegacyPeginKey
    pub fn as_untweaked(&self) -> &bitcoin::PublicKey {
        match *self {
            LegacyPeginKey::Functionary(ref pk) => pk,
            LegacyPeginKey::NonFunctionary(ref pk) => pk,
        }
    }
}

/// 'f' represents tweakable functionary keys and
/// 'u' represents untweakable keys
impl FromStr for LegacyPeginKey {
    // only allow compressed keys in LegacyPegin
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            Err(Error::BadDescriptor(format!("Empty Legacy pegin")))
        } else if &s[0..1] == "f" && s.len() == 67 {
            Ok(LegacyPeginKey::Functionary(bitcoin::PublicKey::from_str(
                &s[1..],
            )?))
        } else if &s[0..1] == "u" && s.len() == 67 {
            Ok(LegacyPeginKey::NonFunctionary(
                bitcoin::PublicKey::from_str(&s[1..])?,
            ))
        } else {
            Err(Error::BadDescriptor(format!(
                "Invalid Legacy Pegin descriptor"
            )))
        }
    }
}

impl fmt::Display for LegacyPeginKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            LegacyPeginKey::Functionary(ref pk) => write!(f, "f{}", pk),
            LegacyPeginKey::NonFunctionary(ref pk) => write!(f, "u{}", pk),
        }
    }
}

impl MiniscriptKey for LegacyPeginKey {
    type Hash = hash160::Hash;

    fn is_uncompressed(&self) -> bool {
        false
    }

    fn serialized_len(&self) -> usize {
        33
    }

    fn to_pubkeyhash(&self) -> Self::Hash {
        let pk = match *self {
            LegacyPeginKey::Functionary(ref pk) => pk,
            LegacyPeginKey::NonFunctionary(ref pk) => pk,
        };
        MiniscriptKey::to_pubkeyhash(pk)
    }
}

/// Legacy Pegin Descriptor
#[derive(Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct LegacyPegin<Pk: MiniscriptKey> {
    /// The federation pks
    pub fed_pks: Vec<LegacyPeginKey>,
    /// The federation threshold
    pub fed_k: usize,
    /// The emergency pks
    pub emer_pks: Vec<LegacyPeginKey>,
    /// The emergency threshold
    pub emer_k: usize,
    /// csv timelock
    pub timelock: u32,
    /// The elements descriptor required to redeem
    pub desc: Descriptor<Pk>,
    // Representation of federation policy as a miniscript
    // Allows for easier implementation
    ms: BtcMiniscript<LegacyPeginKey, BtcSegwitv0>,
}

impl<Pk: MiniscriptKey> LegacyPegin<Pk> {
    /// Create a new LegacyPegin descriptor
    pub fn new(
        fed_pks: Vec<LegacyPeginKey>,
        fed_k: usize,
        emer_pks: Vec<LegacyPeginKey>,
        emer_k: usize,
        timelock: u32,
        desc: Descriptor<Pk>,
    ) -> Self {
        let fed_ms = BtcMiniscript::from_ast(BtcTerminal::Multi(fed_k, fed_pks.clone()))
            .expect("Multi type check can't fail");
        let csv = BtcMiniscript::from_ast(BtcTerminal::Verify(Arc::new(
            BtcMiniscript::from_ast(BtcTerminal::Older(timelock)).unwrap(),
        )))
        .unwrap();
        let emer_ms = BtcMiniscript::from_ast(BtcTerminal::Multi(emer_k, emer_pks.clone()))
            .expect("Multi type check can't fail");
        let emer_ms =
            BtcMiniscript::from_ast(BtcTerminal::AndV(Arc::new(csv), Arc::new(emer_ms))).unwrap();
        let ms = BtcMiniscript::from_ast(BtcTerminal::OrD(Arc::new(fed_ms), Arc::new(emer_ms)))
            .expect("Type check");
        Self {
            fed_pks,
            fed_k,
            emer_pks,
            emer_k,
            timelock,
            desc,
            ms,
        }
    }

    // Internal function to set the fields of Self according to
    // miniscript
    fn from_ms_and_desc(
        desc: Descriptor<Pk>,
        ms: BtcMiniscript<LegacyPeginKey, BtcSegwitv0>,
    ) -> Self {
        // Miniscript is a bunch of Arc's. So, cloning is not as bad.
        // Can we avoid this without NLL?
        let ms_clone = ms.clone();
        let (fed_pks, fed_k, right) = if let BtcTerminal::OrD(ref a, ref b) = ms_clone.node {
            if let (BtcTerminal::Multi(fed_k, fed_pks), right) = (&a.node, &b.node) {
                (fed_pks, *fed_k, right)
            } else {
                unreachable!("Only valid pegin miniscripts");
            }
        } else {
            unreachable!("Only valid pegin miniscripts");
        };
        let (timelock, emer_pks, emer_k) = if let BtcTerminal::AndV(l, r) = right {
            if let (BtcTerminal::Verify(csv), BtcTerminal::Multi(emer_k, emer_pks)) =
                (&l.node, &r.node)
            {
                if let BtcTerminal::Older(timelock) = csv.node {
                    (timelock, emer_pks, *emer_k)
                } else {
                    unreachable!("Only valid pegin miniscripts");
                }
            } else {
                unreachable!("Only valid pegin miniscripts");
            }
        } else {
            unreachable!("Only valid pegin miniscripts");
        };
        Self {
            fed_pks: fed_pks.to_vec(),
            fed_k,
            emer_pks: emer_pks.to_vec(),
            emer_k,
            timelock,
            desc,
            ms,
        }
    }

    /// Create a new descriptor with hard coded values for the
    /// legacy federation and emergency keys
    pub fn new_legacy_fed(user_desc: Descriptor<Pk>) -> Self {
        // Taken from functionary codebase
        // TODO: Verify the keys are correct
        let pks = "
                    020e0338c96a8870479f2396c373cc7696ba124e8635d41b0ea581112b67817261,
                    02675333a4e4b8fb51d9d4e22fa5a8eaced3fdac8a8cbf9be8c030f75712e6af99,
                    02896807d54bc55c24981f24a453c60ad3e8993d693732288068a23df3d9f50d48,
                    029e51a5ef5db3137051de8323b001749932f2ff0d34c82e96a2c2461de96ae56c,
                    02a4e1a9638d46923272c266631d94d36bdb03a64ee0e14c7518e49d2f29bc4010,
                    02f8a00b269f8c5e59c67d36db3cdc11b11b21f64b4bffb2815e9100d9aa8daf07,
                    03079e252e85abffd3c401a69b087e590a9b86f33f574f08129ccbd3521ecf516b,
                    03111cf405b627e22135b3b3733a4a34aa5723fb0f58379a16d32861bf576b0ec2,
                    0318f331b3e5d38156da6633b31929c5b220349859cc9ca3d33fb4e68aa0840174,
                    03230dae6b4ac93480aeab26d000841298e3b8f6157028e47b0897c1e025165de1,
                    035abff4281ff00660f99ab27bb53e6b33689c2cd8dcd364bc3c90ca5aea0d71a6,
                    03bd45cddfacf2083b14310ae4a84e25de61e451637346325222747b157446614c,
                    03cc297026b06c71cbfa52089149157b5ff23de027ac5ab781800a578192d17546,
                    03d3bde5d63bdb3a6379b461be64dad45eabff42f758543a9645afd42f6d424828,
                    03ed1e8d5109c9ed66f7941bc53cc71137baa76d50d274bda8d5e8ffbd6e61fe9a";
        let fed_pks: Vec<LegacyPeginKey> = pks
            .split(",")
            .map(|pk| LegacyPeginKey::Functionary(bitcoin::PublicKey::from_str(pk).unwrap()))
            .collect();

        let emer_pks = "
                    03aab896d53a8e7d6433137bbba940f9c521e085dd07e60994579b64a6d992cf79,
                    0291b7d0b1b692f8f524516ed950872e5da10fb1b808b5a526dedc6fed1cf29807,
                    0386aa9372fbab374593466bc5451dc59954e90787f08060964d95c87ef34ca5bb";
        let emer_pks: Vec<LegacyPeginKey> = emer_pks
            .split(",")
            .map(|pk| LegacyPeginKey::Functionary(bitcoin::PublicKey::from_str(pk).unwrap()))
            .collect();

        Self::new(fed_pks, 11, emer_pks, 2, 4032, user_desc)
    }
}

impl<Pk: MiniscriptKey> fmt::Debug for LegacyPegin<Pk> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "legacy_pegin({:?},{:?})", self.ms, self.desc)
    }
}

impl<Pk: MiniscriptKey> fmt::Display for LegacyPegin<Pk> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let desc = format!("legacy_pegin({},{})", self.ms, self.desc);
        let checksum = desc_checksum(&desc).map_err(|_| fmt::Error)?;
        write!(f, "{}#{}", &desc, &checksum)
    }
}

impl<Pk: MiniscriptKey> Liftable<LegacyPeginKey> for LegacyPegin<Pk> {
    fn lift(&self) -> Result<semantic::Policy<LegacyPeginKey>, Error> {
        let btc_pol = BtcLiftable::lift(&self.ms)?;
        Liftable::lift(&btc_pol)
    }
}

impl<Pk: MiniscriptKey> BtcLiftable<LegacyPeginKey> for LegacyPegin<Pk> {
    fn lift(&self) -> Result<BtcPolicy<LegacyPeginKey>, BtcError> {
        self.ms.lift()
    }
}

impl<Pk: MiniscriptKey> FromTree for LegacyPegin<Pk>
where
    Pk: FromStr,
    Pk::Hash: FromStr,
    <Pk as FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as FromStr>::Err: ToString,
{
    fn from_tree(top: &expression::Tree) -> Result<Self, Error> {
        if top.name == "legacy_pegin" && top.args.len() == 2 {
            // a roundtrip hack to use FromTree from bitcoin::Miniscript from
            // expression::Tree in elements.
            let ms_str = top.args[0].to_string();
            let ms_expr = BtcTree::from_str(&ms_str)?;
            //
            let ms = BtcMiniscript::<LegacyPeginKey, BtcSegwitv0>::from_tree(&ms_expr);
            let desc = Descriptor::<Pk>::from_tree(&top.args[1]);
            Ok(LegacyPegin::from_ms_and_desc(desc?, ms?))
        } else {
            Err(Error::Unexpected(format!(
                "{}({} args) while parsing legacy_pegin descriptor",
                top.name,
                top.args.len(),
            )))
        }
    }
}

impl<Pk: MiniscriptKey> FromStr for LegacyPegin<Pk>
where
    Pk: FromStr,
    Pk::Hash: FromStr,
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

impl<Pk: MiniscriptKey> PeginTrait<Pk> for LegacyPegin<Pk>
where
    Pk: FromStr,
    Pk::Hash: FromStr,
    <Pk as FromStr>::Err: ToString,
    <<Pk as MiniscriptKey>::Hash as FromStr>::Err: ToString,
{
    fn sanity_check(&self) -> Result<(), Error> {
        self.ms
            .sanity_check()
            .map_err(|_| Error::Unexpected(format!("Federation script sanity check failed")))?;
        self.desc
            .sanity_check()
            .map_err(|_| Error::Unexpected(format!("Federation script sanity check failed")))?;
        Ok(())
    }

    fn bitcoin_address<C: secp256k1_zkp::Verification>(
        &self,
        network: bitcoin::Network,
        secp: &secp256k1_zkp::Secp256k1<C>,
    ) -> Result<bitcoin::Address, Error>
    where
        Pk: ToPublicKey,
    {
        Ok(bitcoin::Address::p2shwsh(
            &self.bitcoin_witness_script(secp),
            network,
        ))
    }

    fn bitcoin_script_pubkey<C: secp256k1_zkp::Verification>(
        &self,
        secp: &secp256k1_zkp::Secp256k1<C>,
    ) -> BtcScript
    where
        Pk: ToPublicKey,
    {
        self.bitcoin_address(bitcoin::Network::Bitcoin, secp)
            .expect("Address cannot fail for pegin")
            .script_pubkey()
    }

    fn bitcoin_unsigned_script_sig<C: secp256k1_zkp::Verification>(
        &self,
        secp: &secp256k1_zkp::Secp256k1<C>,
    ) -> BtcScript
    where
        Pk: ToPublicKey,
    {
        let witness_script = self.bitcoin_witness_script(secp);
        script::Builder::new()
            .push_slice(&witness_script.to_v0_p2wsh()[..])
            .into_script()
    }

    fn bitcoin_witness_script<C: secp256k1_zkp::Verification>(
        &self,
        secp: &secp256k1_zkp::Secp256k1<C>,
    ) -> BtcScript
    where
        Pk: ToPublicKey,
    {
        let tweak_vec = self.desc.explicit_script().into_bytes();
        let tweak = hashes::sha256::Hash::hash(&tweak_vec);
        // Hopefully, we never have to use this and dynafed is deployed
        let mut builder = script::Builder::new()
            .push_opcode(opcodes::all::OP_DEPTH)
            .push_int(self.fed_k as i64 + 1)
            .push_opcode(opcodes::all::OP_EQUAL)
            .push_opcode(opcodes::all::OP_IF)
            // manually serialize the left CMS branch, without the OP_CMS
            .push_int(self.fed_k as i64);

        for key in &self.fed_pks {
            let tweaked_pk = tweak_key(key.as_untweaked(), secp, tweak.as_inner());
            builder = builder.push_key(&tweaked_pk);
        }
        let mut nearly_done = builder
            .push_int(self.fed_pks.len() as i64)
            .push_opcode(opcodes::all::OP_ELSE)
            .into_script()
            .to_bytes();

        let right = if let BtcTerminal::OrD(_l, right) = &self.ms.node {
            right
        } else {
            unreachable!("Only valid pegin descriptors should be created inside LegacyPegin")
        };
        let right = right.translate_pk_infallible(
            |pk| pk.as_untweaked().clone(),
            |_| unreachable!("No Keyhashes in legacy pegins"),
        );
        let mut rser = right.encode().into_bytes();
        // ...and we have an OP_VERIFY style checksequenceverify, which in
        // Liquid production was encoded with OP_DROP instead...
        assert_eq!(rser[4], opcodes::all::OP_VERIFY.into_u8());
        rser[4] = opcodes::all::OP_DROP.into_u8();
        // ...then we should serialize it by sharing the OP_CMS across
        // both branches, and add an OP_DEPTH check to distinguish the
        // branches rather than doing the normal cascade construction
        nearly_done.extend(rser);

        let insert_point = nearly_done.len() - 1;
        nearly_done.insert(insert_point, 0x68);
        bitcoin::Script::from(nearly_done)
    }

    fn get_bitcoin_satisfaction<S, C: secp256k1_zkp::Verification>(
        &self,
        secp: &secp256k1_zkp::Secp256k1<C>,
        satisfier: S,
    ) -> Result<(Vec<Vec<u8>>, BtcScript), Error>
    where
        S: BtcSatisfier<bitcoin::PublicKey>,
        Pk: ToPublicKey,
    {
        let tweak_vec = self.desc.explicit_script().into_bytes();
        let tweak = hashes::sha256::Hash::hash(&tweak_vec);
        let unsigned_script_sig = self.bitcoin_unsigned_script_sig(secp);
        let mut sigs = vec![];
        for key in &self.fed_pks {
            let tweaked_pk = tweak_key(key.as_untweaked(), secp, tweak.as_inner());
            match satisfier.lookup_sig(&tweaked_pk) {
                Some(sig) => {
                    let mut sig_vec = sig.0.serialize_der().to_vec();
                    sig_vec.push(sig.1.as_u32() as u8);
                    sigs.push(sig_vec)
                }
                None => {}
            }
        }
        sigs.sort_by(|a, b| a.len().cmp(&b.len()));
        if sigs.len() >= self.fed_k {
            // Prefer using federation keys over emergency paths
            let mut sigs: Vec<Vec<u8>> = sigs.into_iter().take(self.fed_k).collect();
            sigs.push(vec![0]); // CMS extra value
            Ok((sigs, unsigned_script_sig))
        } else {
            let mut emer_sigs = vec![];
            for emer_key in &self.emer_pks {
                match satisfier.lookup_sig(emer_key.as_untweaked()) {
                    Some(sig) => {
                        let mut sig_vec = sig.0.serialize_der().to_vec();
                        sig_vec.push(sig.1.as_u32() as u8);
                        emer_sigs.push(sig_vec)
                    }
                    None => {}
                }
            }
            emer_sigs.sort_by(|a, b| a.len().cmp(&b.len()));
            if emer_sigs.len() >= self.emer_k {
                let mut sigs: Vec<Vec<u8>> = emer_sigs.into_iter().take(self.emer_k).collect();
                sigs.push(vec![0]); // CMS extra value
                Ok((sigs, unsigned_script_sig))
            } else {
                Err(Error::CouldNotSatisfy)
            }
        }
    }

    fn max_satisfaction_weight(&self) -> Result<usize, Error> {
        let script_size = 628;
        Ok(4 * 36
            + varint_len(script_size)
            + script_size
            + varint_len(self.ms.max_satisfaction_witness_elements()?)
            + self.ms.max_satisfaction_size()?)
    }

    fn script_code<C: secp256k1_zkp::Verification>(
        &self,
        secp: &secp256k1_zkp::Secp256k1<C>,
    ) -> BtcScript
    where
        Pk: ToPublicKey,
    {
        self.bitcoin_witness_script(secp)
    }

    fn into_user_descriptor(self) -> Descriptor<Pk> {
        self.desc
    }
}
