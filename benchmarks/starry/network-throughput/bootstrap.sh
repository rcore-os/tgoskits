server_ip=${STARRY_NETWORK_BENCH_SERVER:-}
script_url=${STARRY_NETWORK_BENCH_SCRIPT_URL:-}
source_url=${STARRY_NETWORK_BENCH_SOURCE_URL:-}
curl_url=${STARRY_NETWORK_BENCH_CURL_URL:-}
script=/tmp/network-bench.sh
source=/tmp/network-upload-source
curl_dir=/tmp/network-curl

fetch_asset() {
    fetch_url=$1
    fetch_path=$2
    fetch_attempt=1
    while [ "$fetch_attempt" -le 30 ]; do
        if { { command -v wget >/dev/null 2>&1 &&
                wget -q -T 5 -O "$fetch_path.part" "$fetch_url"; } ||
             { command -v curl >/dev/null 2>&1 &&
                curl --connect-timeout 2 --max-time 5 -fsS "$fetch_url" -o "$fetch_path.part"; }; } &&
            [ -s "$fetch_path.part" ] && mv "$fetch_path.part" "$fetch_path"; then
            return 0
        fi
        sleep 1
        fetch_attempt=$((fetch_attempt + 1))
    done
    return 1
}

prepare_curl() {
    if command -v curl >/dev/null 2>&1; then return 0; fi
    [ -n "$curl_url" ] && mkdir -p "$curl_dir" &&
        fetch_asset "$curl_url" "$curl_dir/curl-bundle.tar.gz" &&
        tar -xzf "$curl_dir/curl-bundle.tar.gz" -C "$curl_dir" || return 1
    PATH="$curl_dir:$PATH"
    export PATH
    command -v curl >/dev/null 2>&1
}

if [ -z "$server_ip" ] || [ -z "$script_url" ] || [ -z "$source_url" ] ||
    ! prepare_curl ||
    ! fetch_asset "$script_url" "$script" ||
    ! fetch_asset "$source_url" "$source" ||
    ! chmod +x "$script" "$source"; then
    echo STARRY_NETWORK_BENCH_FAILED
elif "$script" "$server_ip" "$source"; then
    sync
else
    echo STARRY_NETWORK_BENCH_FAILED
fi
