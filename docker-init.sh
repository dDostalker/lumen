#!/bin/sh
CFGPATH="/dockershare"
DBPATH="/data/lumen.db"
die(){
    echo "Exiting due to error: $@" && exit 1
}
use_default_config(){
    echo "No custom config.toml found, creating secure default."
    mkdir -p "$(dirname "$DBPATH")"
    tee /lumen/config.toml <<- EOF > /dev/null
	[lumina]
	bind_addr = "0.0.0.0:1234"
	use_tls = true
	server_name = "lumen"
	[lumina.tls]
	server_cert = "/lumen/lumen.p12"
	[database]
	path = "${DBPATH}"
	[api_server]
	bind_addr = "0.0.0.0:8082"
	EOF
}
use_default_key(){
    KEYPATH="/lumen/lumen.p12"
    openssl req -x509 -newkey rsa:4096 -keyout /lumen/lumen_key.pem -out /lumen/lumen_crt.pem -days 365 -nodes \
	    --subj "/C=US/ST=Texas/L=Austin/O=Lumina/OU=Naimd/CN=lumen" -passout "pass:" -extensions v3_req || die "Generating key"
    openssl pkcs12 -export -out "${KEYPATH}" -inkey /lumen/lumen_key.pem -in /lumen/lumen_crt.pem  \
	    -passin "pass:" -passout "pass:" || die "Exporting key"
    openssl x509 -in /lumen/lumen_crt.pem -out $CFGPATH/hexrays.crt -passin "pass:" || die "Exporting hexrays.crt"
    echo "hexrays.crt added to mounted volume.  Copy this to your IDA install dir." ;
}
setup_tls_key(){
    PRIVKEY=$(find $CFGPATH -type f \( -name '*.p12' -o -name '*.pfx' \) | head -1)
    PASSIN="-passin pass:$PKCSPASSWD"
    if [ ! -z "${PRIVKEY}" ] ; then
        KEYNAME=$(basename "${PRIVKEY}")
	KEYPATH="/lumen/${KEYNAME}"
        echo "Starting lumen with custom TLS certificate ${KEYNAME}" ;
        cp "${PRIVKEY}" $KEYPATH ;
        openssl pkcs12 -in $KEYPATH ${PASSIN} -clcerts -nokeys -out $CFGPATH/hexrays.crt || die "Exporting hexrays.crt from private key. If there's a password, add it in .env as PKCSPASSWD=...";
        echo "hexrays.crt added to mounted volume.  Copy this to your IDA install dir." ;
        sed -i -e "s,server_cert.*,server_cert = \"${KEYPATH}\"," /lumen/config.toml
    else
        echo "No custom TLS key with p12/pfx extension in the custom mount directory." ;
	use_default_key ;
        sed -i -e "s,server_cert.*,server_cert = \"/lumen/lumen.p12\"," /lumen/config.toml ;
    fi ;
}
setup_config(){
    if [ -e $CFGPATH/config.toml ] ; then
        echo "Detected custom config.toml"
        cp $CFGPATH/config.toml /lumen/config.toml ;
        if grep use_tls /lumen/config.toml | head -1 | grep -q false ; then
            echo "Starting lumen without TLS.  Make sure to set LUMINA_TLS = NO in ida.cfg" ;
        else
	    setup_tls_key ;
        fi ;
    else
	use_default_config ;
	setup_tls_key ;
    fi
    # ensure the configured database file's directory exists
    DBDIR=$(grep -E '^\s*path' /lumen/config.toml | head -1 | sed -E 's,.*=\s*"([^"]+)".*,\1,' | xargs dirname 2>/dev/null || true)
    if [ -n "$DBDIR" ] ; then mkdir -p "$DBDIR" ; fi
}

mkdir -p /data
setup_config ;
echo "Starting lumen. The Turso database file is initialized automatically on first run."
exec lumen -c /lumen/config.toml
