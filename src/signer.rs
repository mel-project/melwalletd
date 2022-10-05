use std::cell::RefCell;

use lru::LruCache;
use themelio_stf::melvm::Covenant;
use themelio_structs::{Transaction, TxHash};
use tmelcrypt::Ed25519SK;



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
            txn.sigs.push(vec![]);
        }
        txn.sigs[input_idx] = signature;
        Ok(txn)
    }

    fn covenant(&self) -> Covenant {
        Covenant::std_ed25519_pk_new(self.to_public())
    }
}
