data "cloudflare_zone" "site" {
  filter = {
    name = cloudflare_workers_custom_domain.worker.zone_name
    account = {
      id = local.cloudflare_account_id
    }
  }
}

resource "cloudflare_ruleset" "zone_custom_firewall" {
  zone_id     = data.cloudflare_zone.site.id
  name        = "Custom firewall rules for white-green.net"
  description = "Exceptions for automated environment uploads"
  kind        = "zone"
  phase       = "http_request_firewall_custom"

  rules = [{
    ref         = "skip_sbfm_environment_upload"
    description = "Skip Super Bot Fight Mode for environment uploads"
    enabled     = true
    expression  = <<-EOT
      (http.host eq "${cloudflare_workers_custom_domain.worker.hostname}"
       and http.request.method eq "PUT"
       and starts_with(http.request.uri.path, "/upload/"))
    EOT
    action      = "skip"
    action_parameters = {
      phases = ["http_request_sbfm"]
    }
    logging = {
      enabled = true
    }
  }]
}
