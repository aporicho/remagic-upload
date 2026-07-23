use super::{meta, set_meta, CatalogError, DeviceIdentity};
use rusqlite::{Connection, TransactionBehavior};
use snow::params::NoiseParams;

pub(super) fn load_or_create_identity(
    connection: &mut Connection,
    name: &str,
) -> Result<DeviceIdentity, CatalogError> {
    let private = meta(connection, "noise_private")?;
    let public = meta(connection, "noise_public")?;
    let (private_key, public_key) = match (private, public) {
        (Some(private), Some(public)) if private.len() == 32 && public.len() == 32 => {
            (private, public)
        }
        (None, None) => {
            let parameters: NoiseParams = "Noise_XX_25519_ChaChaPoly_BLAKE2s".parse()?;
            let keypair = snow::Builder::new(parameters).generate_keypair()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            set_meta(&transaction, "noise_private", &keypair.private)?;
            set_meta(&transaction, "noise_public", &keypair.public)?;
            set_meta(&transaction, "counter", b"0")?;
            transaction.commit()?;
            (keypair.private, keypair.public)
        }
        _ => return Err(CatalogError::CorruptIdentity),
    };
    let id = hex::encode(&blake3::hash(&public_key).as_bytes()[..8]);
    Ok(DeviceIdentity {
        id,
        name: name.to_owned(),
        private_key,
        public_key,
    })
}
