use worker::{Bucket, D1Database};

pub trait GenericConfigProvider {
    fn base_url(&self) -> String;
    fn db(&self) -> D1Database;
    fn bucket(&self) -> Bucket;
}
