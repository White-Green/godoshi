resource "cloudflare_worker" "worker" {
  account_id = local.cloudflare_account_id
  name       = "godoshi-worker"

  lifecycle {
    ignore_changes = all
  }
}

resource "cloudflare_workers_custom_domain" "worker" {
  account_id = local.cloudflare_account_id
  hostname   = "godoshi.white-green.net"
  service    = cloudflare_worker.worker.name
  zone_name  = "white-green.net"
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

resource "cloudflare_queue" "job_queue" {
  account_id = local.cloudflare_account_id
  queue_name = "godoshi-job-queue"
}
