#!/usr/bin/env bash
# Reproducible NativeActivity build. Test signing is explicit and never suitable
# for distribution: use CARGO_APK_RELEASE_KEYSTORE{,_PASSWORD} for your own key.
set -euo pipefail

profile="${1:-debug}"
target="${2:-aarch64-linux-android}"
signing="${3:-}"
case "$profile" in debug|release) ;; *) echo 'Expected debug or release' >&2; exit 2;; esac
case "$target" in aarch64-linux-android|x86_64-linux-android) ;; *) echo 'Expected aarch64-linux-android or x86_64-linux-android' >&2; exit 2;; esac
case "$signing" in ''|--test-signing) ;; *) echo 'Unknown signing option' >&2; exit 2;; esac

project_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$project_root"
export ANDROID_HOME="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export ANDROID_NDK_ROOT="${ANDROID_NDK_ROOT:-$ANDROID_HOME/ndk/27.1.12297006}"
export CARGO_PROFILE_DEV_OPT_LEVEL="${CARGO_PROFILE_DEV_OPT_LEVEL:-1}"
export RUSTC="$(rustup which --toolchain 1.94.1 rustc)"
export RUSTDOC="$(rustup which --toolchain 1.94.1 rustdoc)"
toolchain_bin="$(dirname "$RUSTC")"
export PATH="$toolchain_bin:$HOME/.cargo/bin:$ANDROID_HOME/platform-tools:$PATH"
"$RUSTC" --version
rustup run 1.94.1 cargo --version
if [[ -n "${CARGO_ENCODED_RUSTFLAGS+x}" ]]; then
  export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS}"$'\x1f''-Clink-arg=-Wl,-z,max-page-size=16384'$'\x1f''-Clink-arg=-Wl,-z,common-page-size=16384'
else
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
fi
test -f "$ANDROID_NDK_ROOT/source.properties" || { echo "Missing NDK: $ANDROID_NDK_ROOT" >&2; exit 1; }
grep -q 'Pkg.Revision = 27.1.12297006' "$ANDROID_NDK_ROOT/source.properties" || { echo 'NDK 27.1.12297006 is required' >&2; exit 1; }
apk_version="$(rustup run 1.94.1 cargo apk version)"
[[ "$apk_version" == 'cargo-apk 0.10.0' ]] || { echo 'Install cargo-apk 0.10.0 with --locked' >&2; exit 1; }
rustup target list --installed --toolchain 1.94.1 | grep -qx "$target" || {
  echo "Install target: rustup target add --toolchain 1.94.1 $target" >&2
  exit 1
}

if [[ "$signing" == --test-signing ]]; then
  test_key_directory="$project_root/target/android-test-signing"
  mkdir -p "$test_key_directory"
  test_key="$test_key_directory/debug.keystore"
  if [[ ! -f "$test_key" ]]; then
    keytool -genkeypair -keystore "$test_key" -storepass android -keypass android \
      -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 3650 \
      -dname 'CN=Android Debug,O=Android,C=US' -noprompt
  fi
  export CARGO_APK_RELEASE_KEYSTORE="$test_key"
  export CARGO_APK_RELEASE_KEYSTORE_PASSWORD=android
  export CARGO_APK_DEV_KEYSTORE="$test_key"
  export CARGO_APK_DEV_KEYSTORE_PASSWORD=android
fi

# cargo-apk 0.10.0 does not forward --locked on its build subcommand.
# Validate/fetch the lock first, build offline, and reject any lockfile drift.
rustup run 1.94.1 cargo fetch --locked
lock_hash="$(python3 -c 'import hashlib; print(hashlib.sha256(open("Cargo.lock", "rb").read()).hexdigest())')"
build_args=(build --lib --target "$target")
if [[ "$profile" == release ]]; then build_args+=(--release); fi
CARGO_NET_OFFLINE=true rustup run 1.94.1 cargo apk "${build_args[@]}"
current_lock_hash="$(python3 -c 'import hashlib; print(hashlib.sha256(open("Cargo.lock", "rb").read()).hexdigest())')"
[[ "$lock_hash" == "$current_lock_hash" ]] || { echo 'Cargo.lock changed during packaging' >&2; exit 1; }
apk_path="${CARGO_TARGET_DIR:-$project_root/target}/$profile/apk/kokuban.apk"
ssh_manifest="$project_root/android/ssh-client/Cargo.toml"
ssh_target_directory="${CARGO_TARGET_DIR:-$project_root/target}/ssh-client"
rustup run 1.94.1 cargo fetch --locked --manifest-path "$ssh_manifest"
ssh_args=(-- build --locked --manifest-path "$ssh_manifest" --bin kokuban-ssh --target "$target" --target-dir "$ssh_target_directory")
if [[ "$profile" == release ]]; then ssh_args+=(--release); fi
CARGO_NET_OFFLINE=true rustup run 1.94.1 cargo apk "${ssh_args[@]}"
python3 scripts/android/package.py --apk "$apk_path" --profile "$profile" --target "$target" \
  --ssh-binary "$ssh_target_directory/$target/$profile/kokuban-ssh"
echo "APK: $apk_path"
