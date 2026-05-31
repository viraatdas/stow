//! S3 access: build a client from the standard AWS credential chain, ensure the
//! bucket exists, and put/get objects. All async; driven from the engine's
//! Tokio runtime.

use crate::error::{StowError, StowResult};
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{BucketLocationConstraint, CreateBucketConfiguration};
use aws_sdk_s3::Client;

/// Explicit AWS credentials, captured at init by the unsandboxed CLI and stored
/// in the shared config so the sandboxed extension (which can't read ~/.aws) can
/// use them.
#[derive(Clone)]
pub struct Creds {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// Build an S3 client for `region`. When `creds` is provided, use them directly
/// (the sandboxed path); otherwise fall back to the default chain (`~/.aws`,
/// env, SSO — only works unsandboxed).
pub async fn client(region: &str, creds: Option<Creds>) -> StowResult<Client> {
    // Belt-and-suspenders: never probe EC2 instance metadata (hangs on laptops
    // and in the sandboxed extension). Swift bootstrap sets this too, but set it
    // here so every code path is covered before the SDK loads.
    std::env::set_var("AWS_EC2_METADATA_DISABLED", "true");
    let region_obj = Region::new(region.to_string());
    if let Some(c) = creds {
        let creds = Credentials::new(
            c.access_key_id,
            c.secret_access_key,
            c.session_token,
            None,
            "stow-config",
        );
        let conf = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(region_obj)
            .credentials_provider(creds)
            .load()
            .await;
        Ok(Client::new(&conf))
    } else {
        let conf = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(region_obj)
            .load()
            .await;
        Ok(Client::new(&conf))
    }
}

/// Resolve credentials from the default chain (unsandboxed CLI only) so they can
/// be captured into config at init time.
pub async fn resolve_default_creds(region: &str) -> StowResult<Creds> {
    use aws_credential_types::provider::ProvideCredentials;
    let region_obj = Region::new(region.to_string());
    let conf = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region_obj)
        .load()
        .await;
    let provider = conf
        .credentials_provider()
        .ok_or_else(|| StowError::InvalidConfig("no AWS credential provider".into()))?;
    let c = provider
        .provide_credentials()
        .await
        .map_err(|e| StowError::InvalidConfig(format!("AWS credentials: {e}")))?;
    Ok(Creds {
        access_key_id: c.access_key_id().to_string(),
        secret_access_key: c.secret_access_key().to_string(),
        session_token: c.session_token().map(|s| s.to_string()),
    })
}

/// Return the caller's AWS account id (used to derive a unique bucket name).
pub async fn account_id(region: &str) -> StowResult<String> {
    let region_obj = Region::new(region.to_string());
    let conf = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(region_obj)
        .load()
        .await;
    let sts = aws_sdk_sts::Client::new(&conf);
    let id = sts
        .get_caller_identity()
        .send()
        .await
        .map_err(|e| StowError::Network(format!("sts get-caller-identity: {e:?}")))?;
    id.account()
        .map(|s| s.to_string())
        .ok_or_else(|| StowError::Network("sts returned no account id".into()))
}

/// Create the bucket if it doesn't already exist. Idempotent.
pub async fn ensure_bucket(c: &Client, bucket: &str, region: &str) -> StowResult<()> {
    // Already exists & owned by us?
    if c.head_bucket().bucket(bucket).send().await.is_ok() {
        return Ok(());
    }
    let mut req = c.create_bucket().bucket(bucket);
    // us-east-1 must NOT send a LocationConstraint; all other regions must.
    if region != "us-east-1" {
        // BucketLocationConstraint converts only from &'static str; bucket
        // creation happens once at init, so leaking this short string is fine.
        let static_region: &'static str = Box::leak(region.to_string().into_boxed_str());
        let cfg = CreateBucketConfiguration::builder()
            .location_constraint(BucketLocationConstraint::from(static_region))
            .build();
        req = req.create_bucket_configuration(cfg);
    }
    match req.send().await {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = format!("{e}");
            // Treat "already owned by you" as success (race / re-run).
            if msg.contains("BucketAlreadyOwnedByYou") || msg.contains("BucketAlreadyExists") {
                Ok(())
            } else {
                Err(StowError::Network(format!("create bucket {bucket}: {msg}")))
            }
        }
    }
}

/// Upload bytes to `key`. Skips the upload if the object already exists (dedup).
pub async fn put_object(c: &Client, bucket: &str, key: &str, data: Vec<u8>) -> StowResult<()> {
    if c.head_object().bucket(bucket).key(key).send().await.is_ok() {
        return Ok(()); // content-addressed: identical bytes already stored
    }
    c.put_object()
        .bucket(bucket)
        .key(key)
        .body(ByteStream::from(data))
        .send()
        .await
        .map_err(|e| StowError::Network(format!("put {key}: {e}")))?;
    Ok(())
}

/// Download the full object at `key`.
pub async fn get_object(c: &Client, bucket: &str, key: &str) -> StowResult<Vec<u8>> {
    let out = c
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| StowError::Network(format!("get {key}: {e}")))?;
    let bytes = out
        .body
        .collect()
        .await
        .map_err(|e| StowError::Network(format!("read {key}: {e}")))?;
    Ok(bytes.to_vec())
}

/// Delete an object (used when the last reference to a hash is removed).
pub async fn delete_object(c: &Client, bucket: &str, key: &str) -> StowResult<()> {
    c.delete_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map_err(|e| StowError::Network(format!("delete {key}: {e}")))?;
    Ok(())
}

