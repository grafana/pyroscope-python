package querier

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
)

type Client struct {
	HTTP *http.Client
	URL  string
}

func NewClient(client *http.Client, url string) *Client {
	return &Client{
		HTTP: client,
		URL:  url,
	}
}

func (c *Client) SelectMergeStacktraces(ctx context.Context, req *SelectMergeStacktracesRequest) (*SelectMergeStacktracesResponse, error) {
	url := c.URL + "/querier.v1.QuerierService/SelectMergeStacktraces"
	bs, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal request to json: %w", err)
	}
	httpRequest, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewBuffer(bs))
	if err != nil {
		return nil, fmt.Errorf("failed to create http request: %w", err)
	}
	httpRequest.Header.Set("Content-Type", "application/json")

	resp, err := c.HTTP.Do(httpRequest)
	if err != nil {
		return nil, fmt.Errorf("failed to do http request: %w", err)
	}
	defer resp.Body.Close()
	responseBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response body: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("server returned %d %s", resp.StatusCode, responseBody)
	}
	res := new(SelectMergeStacktracesResponse)
	err = json.Unmarshal(responseBody, res)
	return res, err
}

type ProfileFormat int32

const (
	ProfileFormat_PROFILE_FORMAT_UNSPECIFIED ProfileFormat = 0
	ProfileFormat_PROFILE_FORMAT_FLAMEGRAPH  ProfileFormat = 1
	ProfileFormat_PROFILE_FORMAT_TREE        ProfileFormat = 2
)

type SelectMergeStacktracesRequest struct {
	// Profile Type ID string in the form
	// <name>:<type>:<unit>:<period_type>:<period_unit>.
	ProfileTypeID string `json:"profile_typeID,omitempty"`
	// Label selector string.
	LabelSelector string `json:"label_selector,omitempty"`
	// Milliseconds since epoch.
	Start int64 `json:"start,omitempty"`
	// Milliseconds since epoch.
	End int64 `json:"end,omitempty"`
	// Limit the nodes returned to only show the node with the max_node's biggest
	// total.
	MaxNodes *int64 `json:"max_nodes,omitempty"`
	// Profile format specifies the format to be returned.
	Format ProfileFormat `json:"format,omitempty"`
	// List of Profile UUIDs to query.
	ProfileIDSelector []string `json:"profile_id_selector,omitempty"`
}

type SelectMergeStacktracesResponse struct {
	// Pyroscope tree bytes.
	Tree []byte `json:"tree,omitempty"`
}
