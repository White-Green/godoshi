FROM nixos/nix:2.35.4

# Resolve bootstrap tools at image build time, before outbound S3 interception is available.
RUN nix-env -iA nixpkgs.awscli2 nixpkgs.zstd

WORKDIR /workspace

ENV GODOSHI_S3_ENDPOINT=http://s3.internal \
    NIX_REMOTE=local

COPY entrypoint.sh /godoshi/entrypoint.sh

ENTRYPOINT ["bash", "/godoshi/entrypoint.sh"]
