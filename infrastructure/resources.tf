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

resource "cloudflare_r2_bucket_event_notification" "environment_upload" {
  account_id  = local.cloudflare_account_id
  bucket_name = cloudflare_r2_bucket.runtime_bucket.name
  queue_id    = cloudflare_queue.job_queue.id
  rules = [{
    actions     = ["PutObject"]
    description = "Build after an environment upload completes"
    prefix      = "projects/"
    suffix      = "/resources/environment.closure_zstd"
  }]
}
