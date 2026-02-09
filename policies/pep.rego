package pep

import rego.v1

# Deny all requests by default.
default decision := {
	"allow": false,
	"reason": "denied by default policy",
}

# Allow HTTP requests to explicitly allowlisted domains.
decision := result if {
	input.action.type == "http.request"
	input.action.resource.scheme in {"http", "https"}
	host := input.action.resource.host
	host_allowed(host)
	result := {
		"allow": true,
		"reason": "domain allowlisted",
		"constraints": object.get(data.config, "constraints", {}),
	}
}

# Exact domain match.
host_allowed(host) if {
	some domain in data.config.allowed_domains
	host == domain
}

# Subdomain match (e.g. api.example.com matches example.com).
host_allowed(host) if {
	some domain in data.config.allowed_domains
	endswith(host, concat("", [".", domain]))
}
