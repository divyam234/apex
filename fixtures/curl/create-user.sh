curl -X POST 'https://api.example.test/users' \
  -H 'Content-Type: application/json' \
  -H 'X-Trace: first' \
  -H 'X-Trace: second' \
  --data-raw '{"name":"Ada"}'
