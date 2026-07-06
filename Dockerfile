FROM rust:1.93.1-slim-bookworm
ARG	DEBIAN_FRONTEND=noninteractive
RUN	apt-get update && apt-get install -y --no-install-recommends --no-install-suggests ca-certificates pkg-config libssl-dev
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse

COPY	common	/lumen/common
COPY	lumen	/lumen/lumen
COPY	Cargo.toml /lumen/
RUN --mount=type=cache,target=$CARGO_HOME/registry,target=/lumen/target \
    cd /lumen && cargo build --release && cp /lumen/target/release/lumen /root/

FROM	debian:bookworm-slim
ARG	DEBIAN_FRONTEND=noninteractive
RUN	apt-get update && apt-get install -y --no-install-recommends --no-install-suggests openssl && \
    sed -i -e 's,\[ v3_req \],\[ v3_req \]\nextendedKeyUsage = serverAuth,' /etc/ssl/openssl.cnf
RUN mkdir -p /usr/lib/lumen/

COPY	--from=0	/root/lumen	/usr/bin/lumen

COPY	config-example.toml	docker-init.sh	/lumen/
RUN	chmod a+x /lumen/docker-init.sh && chmod a+x /usr/bin/lumen
WORKDIR	/lumen
CMD /lumen/docker-init.sh
