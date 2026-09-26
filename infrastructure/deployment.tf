data "cloudflare_account_api_token_permission_groups_list" "deployment" {
  account_id = local.cloudflare_account_id
  scope      = "com.cloudflare.api.account"
}

locals {
  deployment_permission_group_ids = {
    for group in data.cloudflare_account_api_token_permission_groups_list.deployment.result :
    group.name => group.id
  }
}

resource "cloudflare_account_token" "wrangler_deploy" {
  account_id = local.cloudflare_account_id
  name       = "godoshi-wrangler-deploy"

  policies = [{
    effect = "allow"
    permission_groups = [
      { id = local.deployment_permission_group_ids["Account Settings Read"] },
      { id = local.deployment_permission_group_ids["Workers Scripts Write"] },
      { id = local.deployment_permission_group_ids["D1 Write"] },
    ]
    resources = jsonencode({
      "com.cloudflare.api.account.${local.cloudflare_account_id}" = "*"
    })
  }]
}

output "wrangler_api_token" {
  description = "API token to use as CLOUDFLARE_API_TOKEN when running wrangler deploy."
  value       = cloudflare_account_token.wrangler_deploy.value
  sensitive   = true
}

output "wrangler_account_id" {
  description = "Cloudflare account ID to use as CLOUDFLARE_ACCOUNT_ID when running wrangler deploy."
  value       = local.cloudflare_account_id
}

output "d1_database_id" {
  description = "ID of the godoshi-db D1 database."
  value       = cloudflare_d1_database.main_db.id
}
