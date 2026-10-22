//! 配对包 `SHW2` / `SHB2` 的编码与 `open_with_pairing`。
//!
//! 外层用 32 字节随机配对 secret 经 Argon2id 派生 KEK 加密；内层携带
//! `db_id`、当前 key 与整条密钥链。外层与 `.design/data.md` §15 的布局一致。

use std::time::Duration;

use redb::Database;
use zeroize::Zeroizing;

use super::*;
use crate::codec2;
use crate::crypto::keys::{DataKey, KEY_LEN};
use crate::error::{Error, Result};
use crate::model::{DeviceId, KeyId, Timestamp};
use crate::store::{catalog, changes, keys2, meta2, records, sync_state, FORMAT_V2};
use crate::sync::{ChangeEntry, DEFAULT_PAIRING_TTL, MAX_KEY_COUNT};

/// 外层魔数。
pub const OUTER_MAGIC: &[u8; 4] = b"SHW2";
/// 内层魔数。
pub const INNER_MAGIC: &[u8; 4] = b"SHB2";
/// 内层格式版本。
pub const INNER_FORMAT: u8 = 2;
/// 外层固定头长度。
pub const OUTER_HEADER_LEN: usize = 4 + 1 + 4 + 4 + 4 + 16;
/// 配对 secret 长度。
pub const SECRET_LEN: usize = 32;
/// 内层固定头长度：magic + format + db + current + app_len + created_at + key_count。
const INNER_FIXED_LEN: usize = 4 + 1 + 16 + 16 + 1 + 8 + 1;
/// 每个密钥条目的长度。
const KEY_ENTRY_LEN: usize = 16 + KEY_LEN;

impl Engine {
    /// 生成配对包；secret 随机生成但只返回帧（底层方法见
    /// [`Self::create_pairing_bundle_with_secret`]）。
    pub fn create_pairing_bundle(&self, ttl: Duration) -> Result<Vec<u8>> {
        let (frame, _secret) = self.create_pairing_bundle_with_secret(ttl)?;
        Ok(frame)
    }

    /// 生成配对包与配对 secret。
    pub fn create_pairing_bundle_with_secret(
        &self,
        ttl: Duration,
    ) -> Result<(Vec<u8>, [u8; SECRET_LEN])> {
        if ttl.is_zero() {
            return Err(Error::InvalidArgument("pairing ttl must be positive"));
        }
        let session = self.read_session()?;
        let state = self.read_sync_state()?.ok_or(Error::SyncDisabled)?;
        if session.format != FORMAT_V2 {
            return Err(Error::SyncDisabled);
        }
        if session.keys.len() > MAX_KEY_COUNT {
            return Err(Error::InvalidArgument("data key count limit reached"));
        }

        let mut secret = [0u8; SECRET_LEN];
        getrandom::fill(&mut secret)
            .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
        let mut salt = [0u8; kdf::SALT_LEN];
        getrandom::fill(&mut salt)
            .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;

        let mut plaintext = Zeroizing::new(Vec::with_capacity(
            INNER_FIXED_LEN + session.app_id.len() + session.keys.len() * KEY_ENTRY_LEN,
        ));
        plaintext.extend_from_slice(INNER_MAGIC);
        plaintext.push(INNER_FORMAT);
        plaintext.extend_from_slice(&state.db_id);
        plaintext.extend_from_slice(session.current.as_bytes());
        let app_id_len = u8::try_from(session.app_id.len())
            .map_err(|_| Error::InvalidArgument("app id exceeds 64 bytes"))?;
        plaintext.push(app_id_len);
        plaintext.extend_from_slice(session.app_id.as_bytes());
        plaintext.extend_from_slice(&Timestamp::now().as_millis().to_be_bytes());
        let key_count = u8::try_from(session.keys.len())
            .map_err(|_| Error::InvalidArgument("data key count limit reached"))?;
        plaintext.push(key_count);
        for (key_id, entry) in &session.keys {
            plaintext.extend_from_slice(key_id.as_bytes());
            plaintext.extend_from_slice(entry.data_key.as_bytes());
        }

        let params = session.kdf_params;
        let kek = kdf::derive_kek(&secret, &salt, &params)?;
        let mut header = Vec::with_capacity(OUTER_HEADER_LEN);
        header.extend_from_slice(OUTER_MAGIC);
        header.push(session.kdf_id);
        header.extend_from_slice(&params.m_cost_kib.to_be_bytes());
        header.extend_from_slice(&params.t_cost.to_be_bytes());
        header.extend_from_slice(&params.p_cost.to_be_bytes());
        header.extend_from_slice(&salt);

        let nonce = Nonce::random()?;
        let mut aad = Vec::with_capacity(codec2::AAD_PAIRING_DOMAIN.len() + header.len());
        aad.extend_from_slice(codec2::AAD_PAIRING_DOMAIN);
        aad.extend_from_slice(&header);
        let ciphertext = aead::seal(kek.as_bytes(), &nonce, &aad, &plaintext)?;
        let ct_len = u32::try_from(ciphertext.len())
            .map_err(|_| Error::Internal("pairing ciphertext length overflow"))?;

        let mut frame = Vec::with_capacity(header.len() + NONCE_LEN + 4 + ciphertext.len());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(nonce.as_bytes());
        frame.extend_from_slice(&ct_len.to_be_bytes());
        frame.extend_from_slice(&ciphertext);
        Ok((frame, secret))
    }

    /// 只创建新库的配对打开；目标必须不存在或为空。
    pub fn open_with_pairing(
        options: EngineOptions,
        password: &[u8],
        bundle: &[u8],
        secret: &[u8],
    ) -> Result<Self> {
        if options.read_only {
            return Err(Error::InvalidArgument(
                "pairing cannot create a read-only database",
            ));
        }
        if options.max_value_size == 0 {
            return Err(Error::InvalidArgument("max_value_size must be positive"));
        }
        if secret.len() != SECRET_LEN {
            return Err(Error::PairingInvalid);
        }

        let path = options.path.clone();
        if path.exists() {
            let metadata = std::fs::metadata(&path).map_err(Error::Io)?;
            if metadata.len() != 0 {
                return Err(Error::InvalidArgument(
                    "pairing target must not exist or must be empty",
                ));
            }
        }

        let result = Self::open_with_pairing_inner(options, password, bundle, secret);
        match result {
            Ok(engine) => Ok(engine),
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                Err(error)
            }
        }
    }

    fn open_with_pairing_inner(
        options: EngineOptions,
        password: &[u8],
        bundle: &[u8],
        secret: &[u8],
    ) -> Result<Self> {
        let parsed = parse_bundle(bundle, secret)?;
        let created_at = parsed.created_at;
        let now = Timestamp::now().as_millis();
        if now.abs_diff(created_at) > DEFAULT_PAIRING_TTL.as_millis() as u64 {
            return Err(Error::PairingExpired);
        }
        if parsed.keys.is_empty() || parsed.keys.len() > MAX_KEY_COUNT {
            return Err(Error::PairingInvalid);
        }
        if !parsed
            .keys
            .iter()
            .any(|(key_id, _)| *key_id == parsed.current)
        {
            return Err(Error::PairingInvalid);
        }

        let mut salt = [0u8; kdf::SALT_LEN];
        getrandom::fill(&mut salt)
            .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
        let params = options.kdf;
        let kek = kdf::derive_kek(password, &salt, &params)?;
        let now_millis = Timestamp::now().as_millis();

        let db = match options.cache_size {
            Some(bytes) => {
                let mut builder = Database::builder();
                builder.set_cache_size(bytes);
                builder.create(&options.path)?
            }
            None => Database::create(&options.path)?,
        };

        let current_entry_dk = parsed
            .keys
            .iter()
            .find(|(key_id, _)| *key_id == parsed.current)
            .map(|(_, dk)| DataKey::from_bytes(*dk))
            .ok_or(Error::PairingInvalid)?;
        let current_subkeys = kdf::derive_subkeys(&current_entry_dk)?;
        let check_nonce = Nonce::random()?;
        let check_ct = aead::seal(
            current_subkeys.check.as_bytes(),
            &check_nonce,
            codec2::keycheck_aad(),
            codec2::KEY_CHECK_PLAINTEXT,
        )?;
        let check_ct: [u8; meta2::KEY_CHECK_CT_LEN] = check_ct
            .as_slice()
            .try_into()
            .map_err(|_| Error::Internal("key check has unexpected length"))?;
        let meta_bytes = meta2::MetaBlob::new(
            parsed.current,
            now_millis,
            &parsed.app_id,
            *check_nonce.as_bytes(),
            check_ct,
        )?
        .encode()?;

        let device = DeviceId::from_bytes(random_16()?);
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(keys2::TABLE)?;
            for (key_id, dk_bytes) in &parsed.keys {
                let wrap_nonce = Nonce::random()?;
                let wrapped = aead::seal(
                    kek.as_bytes(),
                    &wrap_nonce,
                    &codec2::keywrap_aad(*key_id),
                    dk_bytes,
                )?;
                let wrapped_dk: [u8; keys2::WRAPPED_DK_LEN] = wrapped
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::Internal("wrapped data key has unexpected length"))?;
                let created = if *key_id == parsed.current {
                    now_millis
                } else {
                    now_millis.saturating_sub(1)
                };
                let blob = keys2::KeyBlob {
                    key_id: *key_id,
                    kdf_id: kdf::KdfId::Argon2idV1.as_u8(),
                    flags: 0,
                    m_cost_kib: params.m_cost_kib,
                    t_cost: params.t_cost,
                    p_cost: params.p_cost,
                    salt,
                    wrap_nonce: *wrap_nonce.as_bytes(),
                    wrapped_dk,
                    created_at: created,
                    label: "paired".to_owned(),
                };
                table.insert(key_id.as_bytes().as_slice(), blob.encode()?.as_slice())?;
            }
        }
        {
            let mut table = txn.open_table(meta::TABLE)?;
            table.insert(META_KEY, meta_bytes.as_slice())?;
        }
        {
            let mut table = txn.open_table(sync_state::TABLE)?;
            table.insert(
                sync_state::KEY_VERSION,
                sync_state::VERSION.to_be_bytes().as_slice(),
            )?;
            table.insert(sync_state::KEY_DB, parsed.db_id.as_slice())?;
            table.insert(sync_state::KEY_DEVICE, device.as_bytes().as_slice())?;
            table.insert(
                sync_state::KEY_ENABLED_AT,
                now_millis.to_be_bytes().as_slice(),
            )?;
            table.insert(sync_state::KEY_BASELINE_DONE, [1u8].as_slice())?;
            table.insert(sync_state::KEY_LOG_START, 1u64.to_be_bytes().as_slice())?;
            table.insert(
                sync_state::current(device).as_slice(),
                parsed.current.as_bytes().as_slice(),
            )?;
        }
        {
            let mut table = txn.open_table(changes::TABLE)?;
            let bytes = changes::encode(&ChangeEntry::CurrentKey {
                key_id: parsed.current,
                at: Timestamp::from_millis(now_millis),
            })?;
            table.insert(1u64, bytes.as_slice())?;
        }
        {
            let _records = txn.open_table(records::TABLE)?;
            let _catalog = txn.open_table(catalog::TABLE)?;
        }
        txn.commit()?;
        drop(db);

        let open_options = EngineOptions {
            create: false,
            ..options
        };
        Engine::open(open_options, password)
    }
}

struct ParsedBundle {
    db_id: [u8; 16],
    current: KeyId,
    app_id: String,
    created_at: i64,
    keys: Vec<(KeyId, [u8; KEY_LEN])>,
}

fn parse_bundle(bundle: &[u8], secret: &[u8]) -> Result<ParsedBundle> {
    if bundle.len() < OUTER_HEADER_LEN + NONCE_LEN + 4 + codec::TAG_LEN {
        return Err(Error::PairingInvalid);
    }
    if &bundle[0..4] != OUTER_MAGIC {
        return Err(Error::PairingInvalid);
    }
    let kdf_id = bundle[4];
    if kdf_id != kdf::KdfId::Argon2idV1.as_u8() {
        return Err(Error::PairingInvalid);
    }
    let m_cost_kib =
        u32::from_be_bytes(bundle[5..9].try_into().map_err(|_| Error::PairingInvalid)?);
    let t_cost = u32::from_be_bytes(
        bundle[9..13]
            .try_into()
            .map_err(|_| Error::PairingInvalid)?,
    );
    let p_cost = u32::from_be_bytes(
        bundle[13..17]
            .try_into()
            .map_err(|_| Error::PairingInvalid)?,
    );
    let params = kdf::KdfParams::new(m_cost_kib, t_cost, p_cost);
    params.validate().map_err(|_| Error::PairingInvalid)?;
    let salt: [u8; kdf::SALT_LEN] = bundle[17..OUTER_HEADER_LEN]
        .try_into()
        .map_err(|_| Error::PairingInvalid)?;
    let header = &bundle[..OUTER_HEADER_LEN];
    let nonce: [u8; NONCE_LEN] = bundle[OUTER_HEADER_LEN..OUTER_HEADER_LEN + NONCE_LEN]
        .try_into()
        .map_err(|_| Error::PairingInvalid)?;
    let ct_len_offset = OUTER_HEADER_LEN + NONCE_LEN;
    let ct_len = u32::from_be_bytes(
        bundle[ct_len_offset..ct_len_offset + 4]
            .try_into()
            .map_err(|_| Error::PairingInvalid)?,
    );
    let ct_len = usize::try_from(ct_len).map_err(|_| Error::PairingInvalid)?;
    let ct_offset = ct_len_offset + 4;
    let expected = ct_offset.checked_add(ct_len).ok_or(Error::PairingInvalid)?;
    if expected != bundle.len() || ct_len < codec::TAG_LEN {
        return Err(Error::PairingInvalid);
    }

    let kek = kdf::derive_kek(secret, &salt, &params).map_err(|_| Error::PairingInvalid)?;
    let mut aad = Vec::with_capacity(codec2::AAD_PAIRING_DOMAIN.len() + header.len());
    aad.extend_from_slice(codec2::AAD_PAIRING_DOMAIN);
    aad.extend_from_slice(header);
    let nonce = Nonce::from_bytes(nonce);
    let plaintext = Zeroizing::new(
        aead::open(kek.as_bytes(), &nonce, &aad, &bundle[ct_offset..])
            .map_err(|_| Error::PairingInvalid)?,
    );
    parse_inner(&plaintext)
}

fn parse_inner(plaintext: &[u8]) -> Result<ParsedBundle> {
    if plaintext.len() < INNER_FIXED_LEN {
        return Err(Error::PairingInvalid);
    }
    if &plaintext[0..4] != INNER_MAGIC {
        return Err(Error::PairingInvalid);
    }
    if plaintext[4] != INNER_FORMAT {
        return Err(Error::PairingInvalid);
    }
    let db_id: [u8; 16] = plaintext[5..21]
        .try_into()
        .map_err(|_| Error::PairingInvalid)?;
    let current: [u8; 16] = plaintext[21..37]
        .try_into()
        .map_err(|_| Error::PairingInvalid)?;
    let current = KeyId::from_bytes(current);
    if current.is_zero() {
        return Err(Error::PairingInvalid);
    }
    let app_id_len = usize::from(plaintext[37]);
    if app_id_len > meta2::MAX_APP_ID_LEN {
        return Err(Error::PairingInvalid);
    }
    let app_end = 38usize
        .checked_add(app_id_len)
        .ok_or(Error::PairingInvalid)?;
    let app_id = std::str::from_utf8(plaintext.get(38..app_end).ok_or(Error::PairingInvalid)?)
        .map_err(|_| Error::PairingInvalid)?
        .to_owned();
    let created_end = app_end.checked_add(8).ok_or(Error::PairingInvalid)?;
    let created_at = i64::from_be_bytes(
        plaintext
            .get(app_end..created_end)
            .ok_or(Error::PairingInvalid)?
            .try_into()
            .map_err(|_| Error::PairingInvalid)?,
    );
    let key_count = usize::from(*plaintext.get(created_end).ok_or(Error::PairingInvalid)?);
    if key_count == 0 || key_count > MAX_KEY_COUNT {
        return Err(Error::PairingInvalid);
    }
    let entries_offset = created_end + 1;
    let expected = entries_offset
        .checked_add(key_count * KEY_ENTRY_LEN)
        .ok_or(Error::PairingInvalid)?;
    if expected != plaintext.len() {
        return Err(Error::PairingInvalid);
    }

    let mut keys = Vec::with_capacity(key_count);
    for index in 0..key_count {
        let offset = entries_offset + index * KEY_ENTRY_LEN;
        let key_id: [u8; 16] = plaintext[offset..offset + 16]
            .try_into()
            .map_err(|_| Error::PairingInvalid)?;
        let key_id = KeyId::from_bytes(key_id);
        if key_id.is_zero() {
            return Err(Error::PairingInvalid);
        }
        let dk: [u8; KEY_LEN] = plaintext[offset + 16..offset + KEY_ENTRY_LEN]
            .try_into()
            .map_err(|_| Error::PairingInvalid)?;
        if keys.iter().any(|(existing, _)| *existing == key_id) {
            return Err(Error::PairingInvalid);
        }
        keys.push((key_id, dk));
    }

    Ok(ParsedBundle {
        db_id,
        current,
        app_id,
        created_at,
        keys,
    })
}

fn random_16() -> Result<[u8; 16]> {
    let mut bytes = [0u8; 16];
    loop {
        getrandom::fill(&mut bytes)
            .map_err(|_| Error::Internal("operating system CSPRNG unavailable"))?;
        if bytes != [0u8; 16] {
            return Ok(bytes);
        }
    }
}
