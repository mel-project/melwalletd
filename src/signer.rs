use std::cell::RefCell;

use lru::LruCache;
use melstructs::{Transaction, TxHash};
use melvm::Covenant;
use tmelcrypt::Ed25519SK;

/// This trait is implemented by anything "secret key-like" that can sign a transaction. This includes secret keys, password-encumbered secret keys,
pub trait Signer: Send + Sync + 'static {
    /// Given a transaction, returns the signed version. Signing may fail (e.g. due to communication failure).
    fn sign_tx(&self, tx: Transaction, input_idx: usize) -> anyhow::Result<Transaction>;

    /// Covenant that checks for transactions signed with this Signer.
    fn covenant(&self) -> Covenant;
}

/// Signer is implemented for an Ed25519SK. This implements the "new style" of transaction signing, where the ith signature corresponds to the ith input.
impl Signer for Ed25519SK {
    fn sign_tx(&self, mut txn: Transaction, input_idx: usize) -> anyhow::Result<Transaction> {
        thread_local! {
            static CACHE: RefCell<LruCache<TxHash, Vec<u8>>> = RefCell::new(LruCache::new(500))
        }

        let signature = CACHE.with(|rc| {
            let mut rc = rc.borrow_mut();
            let h = txn.hash_nosigs();
            rc.get_or_insert(h, || self.sign(&h.0)).unwrap().clone()
        });
        // fill any previous signature slots with zeros
        while txn.sigs.len() <= input_idx {
            txn.sigs.push(Default::default());
        }
        txn.sigs[input_idx] = signature.into();
        Ok(txn)
    }

    fn covenant(&self) -> Covenant {
        Covenant::std_ed25519_pk_new(self.to_public())
    }
}
