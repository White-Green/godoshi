resource "cloudflare_worker" "example_worker" {
  account_id = local.cloudflare_account_id
  name = "godoshi-worker"
}

resource "cloudflare_d1_database" "example_d1_database" {
  account_id = local.cloudflare_account_id
  name = "godoshi-db"
}

resource "cloudflare_r2_bucket" "example_r2_bucket" {
  account_id = local.cloudflare_account_id
  name = "godoshi-runtime-bucket"
}
