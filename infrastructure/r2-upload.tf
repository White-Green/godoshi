data "cloudflare_account_api_token_permission_groups_list" "r2_bucket_items" {
  account_id = local.cloudflare_account_id
  scope      = "com.cloudflare.edge.r2.bucket"
}

locals {
  r2_bucket_item_permission_group_ids = {
    for group in data.cloudflare_account_api_token_permission_groups_list.r2_bucket_items.result :
    group.name => group.id
  }
}

resource "cloudflare_account_token" "environment_upload" {
  account_id = local.cloudflare_account_id
  name       = "godoshi-environment-upload"

  policies = [{
    effect = "allow"
    permission_groups = [
      { id = local.r2_bucket_item_permission_group_ids["Workers R2 Storage Bucket Item Write"] },
    ]
    resources = jsonencode({
      "com.cloudflare.edge.r2.bucket.${local.cloudflare_account_id}_default_${cloudflare_r2_bucket.runtime_bucket.name}" = "*"
    })
  }]
}

# R2's S3 credentials are the token ID and the SHA-256 hash of its value.
# This JSON object can be piped directly to `wrangler secret bulk`.
output "r2_environment_upload_secrets" {
  description = "R2 credentials for the Worker's environment upload signer, in Wrangler secret bulk format."
  value = {
    R2_ACCESS_KEY_ID     = cloudflare_account_token.environment_upload.id
    R2_SECRET_ACCESS_KEY = sha256(cloudflare_account_token.environment_upload.value)
  }
  sensitive = true
}
