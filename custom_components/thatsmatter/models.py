"""Export and catalog data models (protocol-aligned)."""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any
from uuid import uuid4

from .helpers import (
    FIRST_SHIP_TYPES,
    export_to_protocol_dict,
    validate_export_fields,
)


@dataclass
class Export:
    """One curated export (device as controllers see it)."""

    export_id: str
    name: str
    type: str
    primary_entity_id: str
    linked: dict[str, str] = field(default_factory=dict)
    area_id: str | None = None
    enabled: bool = True
    endpoint_id: int | None = None

    def to_protocol_dict(self) -> dict[str, Any]:
        """Serialize to protocol Export JSON."""
        return export_to_protocol_dict(asdict(self))

    def to_storage_dict(self) -> dict[str, Any]:
        """Serialize for HA .storage (same shape as protocol)."""
        return self.to_protocol_dict()

    @staticmethod
    def from_protocol_dict(data: dict[str, Any]) -> Export:
        """Parse protocol or storage Export dict."""
        linked_raw = data.get("linked") or {}
        linked = {str(k): str(v) for k, v in linked_raw.items()}
        return Export(
            export_id=str(data["export_id"]),
            name=str(data["name"]),
            type=str(data["type"]),
            primary_entity_id=str(data["primary_entity_id"]),
            linked=linked,
            area_id=data.get("area_id"),
            enabled=bool(data.get("enabled", True)),
            endpoint_id=data.get("endpoint_id"),
        )

    @staticmethod
    def new(
        *,
        name: str,
        type_key: str,
        primary_entity_id: str,
        linked: dict[str, str] | None = None,
        area_id: str | None = None,
        enabled: bool = True,
        export_id: str | None = None,
    ) -> Export:
        """Create a new export with a stable UUID identity."""
        errors = validate_export_fields(
            name=name,
            type_key=type_key,
            primary_entity_id=primary_entity_id,
            linked=linked,
        )
        if errors:
            raise ValueError("; ".join(errors))
        if type_key not in FIRST_SHIP_TYPES:
            raise ValueError(f"unsupported type: {type_key}")
        return Export(
            export_id=export_id or str(uuid4()),
            name=name.strip(),
            type=type_key,
            primary_entity_id=primary_entity_id,
            linked=dict(linked or {}),
            area_id=area_id,
            enabled=enabled,
            endpoint_id=None,
        )


@dataclass
class CatalogData:
    """HA-owned catalog snapshot stored under .storage."""

    bridge_name: str
    exports: list[Export] = field(default_factory=list)

    def to_storage_dict(self) -> dict[str, Any]:
        """Serialize for HA Store."""
        return {
            "bridge_name": self.bridge_name,
            "exports": [e.to_storage_dict() for e in self.exports],
        }

    @staticmethod
    def from_storage_dict(data: dict[str, Any] | None, default_bridge_name: str) -> CatalogData:
        """Load from storage payload or empty defaults."""
        if not data:
            return CatalogData(bridge_name=default_bridge_name, exports=[])
        exports = [
            Export.from_protocol_dict(item) for item in data.get("exports") or []
        ]
        name = str(data.get("bridge_name") or default_bridge_name)
        return CatalogData(bridge_name=name, exports=exports)

    def get(self, export_id: str) -> Export | None:
        """Return export by id or None."""
        for exp in self.exports:
            if exp.export_id == export_id:
                return exp
        return None

    def upsert(self, export: Export) -> None:
        """Insert or replace by export_id."""
        for i, existing in enumerate(self.exports):
            if existing.export_id == export.export_id:
                self.exports[i] = export
                return
        self.exports.append(export)

    def delete(self, export_id: str) -> Export | None:
        """Remove and return the export, or None if missing."""
        for i, existing in enumerate(self.exports):
            if existing.export_id == export_id:
                return self.exports.pop(i)
        return None

    def list_exports(self) -> list[Export]:
        """Return a copy of the export list."""
        return list(self.exports)

    def enabled_primary_entity_ids(self) -> set[str]:
        """Entity ids that should receive state_changed handling."""
        ids: set[str] = set()
        for exp in self.exports:
            if not exp.enabled:
                continue
            ids.add(exp.primary_entity_id)
            ids.update(exp.linked.values())
        return ids

    def find_by_entity(self, entity_id: str) -> list[Export]:
        """Enabled exports that use entity_id as primary or linked."""
        matches: list[Export] = []
        for exp in self.exports:
            if not exp.enabled:
                continue
            if exp.primary_entity_id == entity_id or entity_id in exp.linked.values():
                matches.append(exp)
        return matches
