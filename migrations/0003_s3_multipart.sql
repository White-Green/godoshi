create table s3_multipart_upload
(
    -- The physical key includes projects/<project_id>/result/ and scopes the upload.
    object_key   text not null,
    r2_upload_id text not null,
    -- checksum::Algorithm: 1 = CRC32, 2 = CRC64NVME, NULL = not specified at creation.
    algorithm    integer check (algorithm in (1, 2)),
    primary key (object_key, r2_upload_id)
);

create table s3_multipart_part
(
    object_key   text    not null,
    r2_upload_id text    not null,
    part_number  integer not null check (part_number between 1 and 10000),
    etag         text    not null,
    length       integer not null check (length between 0 and 5368709120),
    -- checksum::Algorithm: 1 = CRC32, 2 = CRC64NVME.
    algorithm    integer not null check (algorithm in (1, 2)),
    checksum     text    not null,
    -- Keep each returned ETag: concurrent retries may persist in a different
    -- order from R2's writes to the same part number.
    primary key (object_key, r2_upload_id, part_number, etag),
    foreign key (object_key, r2_upload_id) references s3_multipart_upload (object_key, r2_upload_id) on delete cascade
);
