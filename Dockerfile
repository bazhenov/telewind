FROM quay.io/centos/centos:stream9

RUN yum install -y compat-openssl11 && \
    yum clean all && \
    rm -rf /var/cache/yum

LABEL org.opencontainers.image.source https://github.com/bazhenov/telewind

ADD target/container/telewind /opt/telewind
ENTRYPOINT ["/opt/telewind"]