resource "cloudflare_worker" "worker" {
  account_id = local.cloudflare_account_id
  name       = "godoshi-worker"
}

resource "cloudflare_d1_database" "main_db" {
  account_id = local.cloudflare_account_id
  name       = "godoshi-db"
  read_replication = {
    mode = "disabled"
  }
}

resource "cloudflare_r2_bucket" "runtime_bucket" {
  account_id = local.cloudflare_account_id
  name       = "godoshi-runtime-bucket"
}
