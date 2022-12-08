FROM namshi/smtp

ENV CERTIFICATE_PATH=/selfsigned.crt
ENV KEY_PATH=/selfsigned.key
ENV PORT=1026
ENV SMARTHOST_ADDRESS=smtp
ENV SMARTHOST_PORT=1025

COPY asset/selfsigned.key /selfsigned.key
COPY asset/selfsigned.crt /selfsigned.crt
