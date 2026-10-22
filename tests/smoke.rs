//! 跨 crate 冒烟测试：验证 facade 与 core 的公共面在同一 workspace 内可用。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use shio::{
    validate_name, Credential, Error, ListOptions, NameKind, OpenOptions, Page, RecordId,
    RecordMeta, Timestamp, DEFAULT_LIST_LIMIT, MAX_NAME_LEN,
};
use shio_core::codec::{decode_record_key, encode_record_key, RecordHeader, HEADER_LEN};
use shio_core::crypto::keys::KeyId;

#[test]
fn facade_and_core_share_the_same_model() {
    let id = RecordId::new_v7();
    let key = encode_record_key("notes", "note", &id).unwrap();
    let (namespace, collection, restored) = decode_record_key(&key).unwrap();
    assert_eq!(namespace, "notes");
    assert_eq!(collection, "note");
    assert_eq!(restored, id);

    let meta = RecordMeta {
        id,
        rev: 1,
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
        data_version: 1,
        deleted: false,
        ciphertext_len: 32,
    };
    let page: Page<RecordMeta> = Page {
        items: vec![meta],
        next: None,
    };
    assert_eq!(page.items.len(), 1);
}

#[test]
fn codec_header_matches_core_contract() {
    let header = RecordHeader::new(KeyId::new(1).unwrap().get(), 1, 0, 0, 1);
    let encoded = header.encode();
    assert_eq!(encoded.len(), HEADER_LEN);
    assert_eq!(RecordHeader::decode(&encoded).unwrap(), header);
}

#[test]
fn facade_validates_before_touching_disk() {
    let options = OpenOptions::new("./data/shio.redb").create(true);
    options.validate().unwrap();

    let credential = Credential::password("correct horse battery staple");
    assert_eq!(
        format!("{credential:?}"),
        "Credential::Password(<redacted>)"
    );

    assert!(validate_name(NameKind::Namespace, "notes").is_ok());
    assert!(matches!(
        validate_name(NameKind::Namespace, "Notes"),
        Err(Error::InvalidName {
            kind: NameKind::Namespace,
            ..
        })
    ));
    assert_eq!(ListOptions::default().limit, DEFAULT_LIST_LIMIT);
    assert_eq!("a".repeat(MAX_NAME_LEN + 1).len(), MAX_NAME_LEN + 1);
}
