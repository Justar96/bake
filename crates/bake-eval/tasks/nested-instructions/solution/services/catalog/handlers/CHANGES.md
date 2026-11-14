# Catalog handler changes

## Unreleased

### Added
- GET /search/{term}: the products whose name contains the term
- GET /products/{id}/stock: the units of one product that can be sold now

### Changed
- GET /products: lists the ids in pages of 20
- GET /products/{id}: includes in_stock, the units that can be sold now

## 1.4.0

### Added
- GET /categories/{id}/products: the products of one category
- GET /products/{id}: one product
