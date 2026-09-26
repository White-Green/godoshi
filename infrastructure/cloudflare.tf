variable "cloudflare_api_token" {
  type = string
}

variable "cloudflare_account_id" {
  type = string
}

provider "cloudflare" {
  api_token = var.cloudflare_api_token
}

locals {
  cloudflare_account_id = var.cloudflare_account_id
}
