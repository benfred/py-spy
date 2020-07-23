#!/usr/bin/env bash
set -ex

if [[ "$CIRRUS_RELEASE" == "" ]]; then
  echo "Not a release. No need to deploy!"
  exit 0
fi

if [[ "$GITHUB_TOKEN" == "" ]]; then
  echo "Please provide GitHub access token via GITHUB_TOKEN environment variable!"
  exit 1
fi

file_content_type="application/octet-stream"
fpath=py-spy-$CIRRUS_TAG-x86_64-freebsd.tar.gz

tar -C ./target/release -czf $fpath py-spy

echo "Uploading $fpath..."
name=$(basename "$fpath")
url_to_upload="https://uploads.github.com/repos/$CIRRUS_REPO_FULL_NAME/releases/$CIRRUS_RELEASE/assets?name=$name"
curl -X POST \
--data-binary @$fpath \
--header "Authorization: token $GITHUB_TOKEN" \
--header "Content-Type: $file_content_type" \
$url_to_upload
