package pep_test

import rego.v1
import data.pep

# Requests to unknown domains are denied by default.
test_deny_unknown_domain if {
	result := pep.decision with input as {
		"action": {
			"type": "http.request",
			"resource": {
				"host": "evil.com",
				"scheme": "https",
				"url": "https://evil.com/",
				"method": "GET",
				"path": "/",
			},
		},
		"subject": {"user_id": "test", "workspace_id": "test"},
		"context": {"time": "0", "stage": "test", "mode": "test"},
	}
		with data.config as {
			"allowed_domains": ["example.com"],
			"constraints": {"max_bytes": 1048576},
		}
	result.allow == false
}

# Requests to allowlisted domains succeed.
test_allow_listed_domain if {
	result := pep.decision with input as {
		"action": {
			"type": "http.request",
			"resource": {
				"host": "example.com",
				"scheme": "https",
				"url": "https://example.com/",
				"method": "GET",
				"path": "/",
			},
		},
		"subject": {"user_id": "test", "workspace_id": "test"},
		"context": {"time": "0", "stage": "test", "mode": "test"},
	}
		with data.config as {
			"allowed_domains": ["example.com"],
			"constraints": {"max_bytes": 1048576},
		}
	result.allow == true
}

# Subdomains of allowlisted domains succeed.
test_allow_subdomain if {
	result := pep.decision with input as {
		"action": {
			"type": "http.request",
			"resource": {
				"host": "api.example.com",
				"scheme": "https",
				"url": "https://api.example.com/",
				"method": "GET",
				"path": "/",
			},
		},
		"subject": {"user_id": "test", "workspace_id": "test"},
		"context": {"time": "0", "stage": "test", "mode": "test"},
	}
		with data.config as {
			"allowed_domains": ["example.com"],
			"constraints": {"max_bytes": 1048576},
		}
	result.allow == true
}

# Non-HTTP schemes are denied even for allowlisted domains.
test_deny_non_http_scheme if {
	result := pep.decision with input as {
		"action": {
			"type": "http.request",
			"resource": {
				"host": "example.com",
				"scheme": "ftp",
				"url": "ftp://example.com/",
				"method": "GET",
				"path": "/",
			},
		},
		"subject": {"user_id": "test", "workspace_id": "test"},
		"context": {"time": "0", "stage": "test", "mode": "test"},
	}
		with data.config as {
			"allowed_domains": ["example.com"],
			"constraints": {},
		}
	result.allow == false
}

# Non-HTTP action types are denied.
test_deny_non_http_action if {
	result := pep.decision with input as {
		"action": {
			"type": "file.write",
			"resource": {"path": "/etc/passwd"},
		},
		"subject": {"user_id": "test", "workspace_id": "test"},
		"context": {"time": "0", "stage": "test", "mode": "test"},
	}
		with data.config as {
			"allowed_domains": ["example.com"],
			"constraints": {},
		}
	result.allow == false
}

# Empty allowlist denies everything.
test_deny_empty_allowlist if {
	result := pep.decision with input as {
		"action": {
			"type": "http.request",
			"resource": {
				"host": "example.com",
				"scheme": "https",
				"url": "https://example.com/",
				"method": "GET",
				"path": "/",
			},
		},
		"subject": {"user_id": "test", "workspace_id": "test"},
		"context": {"time": "0", "stage": "test", "mode": "test"},
	}
		with data.config as {
			"allowed_domains": [],
			"constraints": {},
		}
	result.allow == false
}
