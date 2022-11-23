#!/usr/bin/env bash
set -e

docker-compose up --no-start dummy
docker-compose run cargo build --locked --release
mkdir -p ./target/container
docker-compose cp dummy:/opt/target/release/telewind ./target/container/telewind
docker build . -t ghcr.io/bazhenov/telewind:dev

if [[ ! -z "$1" ]]; then
  source .env
  echo $GITHUB_TOKEN | docker login ghcr.io -u not-required --password-stdin
  docker tag ghcr.io/bazhenov/telewind:dev "ghcr.io/bazhenov/telewind:$1"
  docker push "ghcr.io/bazhenov/telewind:$1"
fi

docker-compose down