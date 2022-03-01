use themelio_stf::melvm::Covenant;
use themelio_structs::Transaction;
use tmelcrypt::Ed25519SK;

/// This trait is implemented by anything "secret key-like" that can sign a transaction. This includes secret keys, password-encumbered secret keys,
pub trait Signer: Send + Sync + 'static {
    /// Given a transaction, returns the signed version. Signing may fail (e.g. due to communication failure).
    fn sign_tx(&self, txn: Transaction, input_idx: usize) -> anyhow::Result<Transaction>;

    /// Covenant that checks for transactions signed with this Signer.
    fn covenant(&self) -> Covenant;
}

/// Signer is implemented for an Ed25519SK. This implements the "new style" of transaction signing, where the ith signature corresponds to the ith input.
impl Signer for Ed25519SK {
    fn sign_tx(&self, mut txn: Transaction, input_idx: usize) -> anyhow::Result<Transaction> {
        let signature = self.sign(&txn.hash_nosigs().0);
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
