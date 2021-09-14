FROM namshi/smtp

COPY asset/selfsigned.key /selfsigned.key
COPY asset/selfsigned.crt /selfsigned.crt

ENV CERTIFICATE_PATH=/selfsigned.crt
ENV KEY_PATH=/selfsigned.key
