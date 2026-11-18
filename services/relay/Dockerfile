FROM scratch
COPY --chmod=755 iroh-relay /iroh-relay
COPY relay.toml /etc/iroh-relay.toml
USER 65532:65532
EXPOSE 3340/tcp
ENTRYPOINT ["/iroh-relay"]
CMD ["--config-path", "/etc/iroh-relay.toml"]
