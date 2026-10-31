package fakestorage

import (
	"encoding/json"
	"time"

	"cloud.google.com/go/storage"
)

type attrsJSON ObjectAttrs

type projectTeamJSON struct {
	ProjectNumber string `json:"projectNumber"`
	Team          string `json:"team"`
}

type aclJSON struct {
	Entity      storage.ACLEntity `json:"entity"`
	EntityID    string            `json:"entityId"`
	Role        storage.ACLRole   `json:"role"`
	Domain      string            `json:"domain"`
	Email       string            `json:"email"`
	ProjectTeam *projectTeamJSON  `json:"projectTeam"`
}

type retentionJSON struct {
	Mode        string    `json:"mode,omitempty"`
	RetainUntil time.Time `json:"retainUntilTime,omitempty"`
}

type objectJSON struct {
	*attrsJSON
	ACL       []aclJSON      `json:"acl,omitempty"`
	Retention *retentionJSON `json:"retention,omitempty"`
}

func (o ObjectAttrs) MarshalJSON() ([]byte, error) {
	value := objectJSON{attrsJSON: (*attrsJSON)(&o), ACL: encodeACL(o.ACL)}
	if o.Retention != nil {
		value.Retention = &retentionJSON{Mode: o.Retention.Mode, RetainUntil: o.Retention.RetainUntil}
	}
	return json.Marshal(value)
}

func encodeACL(rules []storage.ACLRule) []aclJSON {
	var entries []aclJSON
	for _, rule := range rules {
		acl := aclJSON{Entity: rule.Entity, EntityID: rule.EntityID, Role: rule.Role, Domain: rule.Domain, Email: rule.Email}
		if rule.ProjectTeam != nil {
			acl.ProjectTeam = &projectTeamJSON{ProjectNumber: rule.ProjectTeam.ProjectNumber, Team: rule.ProjectTeam.Team}
		}
		entries = append(entries, acl)
	}
	return entries
}

func (o *ObjectAttrs) UnmarshalJSON(data []byte) error {
	var attrs attrsJSON
	value := objectJSON{attrsJSON: &attrs}
	if err := json.Unmarshal(data, &value); err != nil {
		return err
	}
	for _, rule := range value.ACL {
		acl := storage.ACLRule{Entity: rule.Entity, EntityID: rule.EntityID, Role: rule.Role, Domain: rule.Domain, Email: rule.Email}
		if rule.ProjectTeam != nil {
			acl.ProjectTeam = &storage.ProjectTeam{ProjectNumber: rule.ProjectTeam.ProjectNumber, Team: rule.ProjectTeam.Team}
		}
		attrs.ACL = append(attrs.ACL, acl)
	}
	if value.Retention != nil {
		attrs.Retention = &storage.ObjectRetention{Mode: value.Retention.Mode, RetainUntil: value.Retention.RetainUntil}
	}
	*o = ObjectAttrs(attrs)
	return nil
}
