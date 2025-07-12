#![allow(unexpected_cfgs)]
#![allow(deprecated)]
use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer, Mint};
use anchor_spl::associated_token::AssociatedToken;

declare_id!("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS");

// Import the marketplace module properly
pub mod marketplace {
    use super::*;
    
    #[account]
    #[derive(InitSpace)]
    pub struct MarketplaceState {
        pub authority: Pubkey,
        pub treasury: Pubkey,
        pub fee_basis_points: u16,
        pub is_paused: bool,
        pub total_sales: u64,
        pub total_volume: u64,
        pub bump: u8,
    }
    
    impl MarketplaceState {
        pub fn calculate_platform_fee(&self, sale_price: u64) -> Result<u64> {
            let fee = (sale_price as u128)
                .checked_mul(self.fee_basis_points as u128)
                .ok_or(crate::ListingError::MathOverflow)?
                .checked_div(10000)
                .ok_or(crate::ListingError::MathOverflow)? as u64;
            Ok(fee)
        }
    }
    
    pub mod cpi {
        use super::*;
        
        pub mod accounts {
            use super::*;
            
            #[derive(Accounts)]
            pub struct UpdateStats<'info> {
                #[account(mut)]
                pub marketplace: Account<'info, super::MarketplaceState>,
            }
        }
        
        pub fn update_stats<'info>(
            mut ctx: CpiContext<'_, '_, '_, 'info, accounts::UpdateStats<'info>>, 
            sale_amount: u64
        ) -> Result<()> {
            let marketplace = &mut ctx.accounts.marketplace;
            marketplace.total_sales = marketplace.total_sales.checked_add(1)
                .ok_or(crate::ListingError::MathOverflow)?;
            marketplace.total_volume = marketplace.total_volume.checked_add(sale_amount)
                .ok_or(crate::ListingError::MathOverflow)?;
            Ok(())
        }
    }
}

// Declare the marketplace program ID separately

#[program]
pub mod listing {
    use super::*;

    /// List an NFT for fixed price sale
    pub fn list_nft(
        ctx: Context<ListNft>,
        price: u64,
        expiry: Option<i64>,
    ) -> Result<()> {
        // Validate marketplace is active
        require!(!ctx.accounts.marketplace.is_paused, ListingError::MarketplacePaused);
        require!(price > 0, ListingError::InvalidPrice);
        
        // Validate expiry if provided
        if let Some(expiry_time) = expiry {
            let clock = Clock::get()?;
            require!(expiry_time > clock.unix_timestamp, ListingError::InvalidExpiry);
        }

        // Transfer NFT to listing escrow
        let transfer_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.seller_token_account.to_account_info(),
                to: ctx.accounts.listing_token_account.to_account_info(),
                authority: ctx.accounts.seller.to_account_info(),
            },
        );
        token::transfer(transfer_ctx, 1)?;

        // Initialize listing state
        let listing = &mut ctx.accounts.listing;
        listing.seller = ctx.accounts.seller.key();
        listing.mint = ctx.accounts.mint.key();
        listing.price = price;
        listing.created_at = Clock::get()?.unix_timestamp;
        listing.expiry = expiry;
        listing.is_active = true;
        listing.bump = ctx.bumps.listing;

        emit!(NftListed {
            listing: listing.key(),
            seller: listing.seller,
            mint: listing.mint,
            price,
            expiry,
        });

        Ok(())
    }

    /// Update listing price (only seller)
    pub fn update_listing(
        ctx: Context<UpdateListing>,
        new_price: u64,
        new_expiry: Option<i64>,
    ) -> Result<()> {
        require!(new_price > 0, ListingError::InvalidPrice);
        require!(ctx.accounts.listing.is_active, ListingError::ListingNotActive);
        
        // Validate expiry if provided
        if let Some(expiry_time) = new_expiry {
            let clock = Clock::get()?;
            require!(expiry_time > clock.unix_timestamp, ListingError::InvalidExpiry);
        }

        let listing = &mut ctx.accounts.listing;
        let old_price = listing.price;
        listing.price = new_price;
        listing.expiry = new_expiry;

        emit!(ListingUpdated {
            listing: listing.key(),
            seller: listing.seller,
            old_price,
            new_price,
            new_expiry,
        });

        Ok(())
    }

    /// Cancel listing and return NFT to seller
    pub fn cancel_listing(ctx: Context<CancelListing>) -> Result<()> {
        require!(ctx.accounts.listing.is_active, ListingError::ListingNotActive);

        // Transfer NFT back to seller
        let listing_key = ctx.accounts.listing.key();
        let seeds = &[
            b"listing",
            listing_key.as_ref(),
            &[ctx.accounts.listing.bump],
        ];
        let signer = &[&seeds[..]];

        let transfer_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.listing_token_account.to_account_info(),
                to: ctx.accounts.seller_token_account.to_account_info(),
                authority: ctx.accounts.listing.to_account_info(),
            },
            signer,
        );
        token::transfer(transfer_ctx, 1)?;

        // Mark listing as inactive
        let listing = &mut ctx.accounts.listing;
        listing.is_active = false;

        emit!(ListingCanceled {
            listing: listing.key(),
            seller: listing.seller,
            mint: listing.mint,
        });

        Ok(())
    }

    /// Buy NFT from listing
    pub fn buy_nft(ctx: Context<BuyNft>) -> Result<()> {
        let listing = &ctx.accounts.listing;
        require!(listing.is_active, ListingError::ListingNotActive);
        require!(!ctx.accounts.marketplace.is_paused, ListingError::MarketplacePaused);

        // Check if listing has expired
        if let Some(expiry) = listing.expiry {
            let clock = Clock::get()?;
            require!(clock.unix_timestamp <= expiry, ListingError::ListingExpired);
        }

        let sale_price = listing.price;
        
        // Calculate platform fee
        let platform_fee = ctx.accounts.marketplace.calculate_platform_fee(sale_price)?;
        let seller_proceeds = sale_price.checked_sub(platform_fee)
            .ok_or(ListingError::MathOverflow)?;

        // Transfer platform fee to treasury
        if platform_fee > 0 {
            let fee_transfer_ctx = CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.buyer.to_account_info(),
                    to: ctx.accounts.treasury.to_account_info(),
                },
            );
            anchor_lang::system_program::transfer(fee_transfer_ctx, platform_fee)?;
        }

        // Transfer payment to seller (minus platform fee)
        let payment_transfer_ctx = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            anchor_lang::system_program::Transfer {
                from: ctx.accounts.buyer.to_account_info(),
                to: ctx.accounts.seller.to_account_info(),
            },
        );
        anchor_lang::system_program::transfer(payment_transfer_ctx, seller_proceeds)?;

        // Transfer NFT to buyer
        let listing_key = listing.key();
        let seeds = &[
            b"listing",
            listing_key.as_ref(),
            &[listing.bump],
        ];
        let signer = &[&seeds[..]];

        let nft_transfer_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.listing_token_account.to_account_info(),
                to: ctx.accounts.buyer_token_account.to_account_info(),
                authority: ctx.accounts.listing.to_account_info(),
            },
            signer,
        );
        token::transfer(nft_transfer_ctx, 1)?;

        // Mark listing as inactive
        let listing = &mut ctx.accounts.listing;
        listing.is_active = false;

        // Update marketplace stats via CPI
        let cpi_accounts = marketplace::cpi::accounts::UpdateStats {
            marketplace: ctx.accounts.marketplace.clone(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.marketplace_program.to_account_info(), cpi_accounts);
        marketplace::cpi::update_stats(cpi_ctx, sale_price)?;

        emit!(NftSold {
            listing: listing.key(),
            seller: listing.seller,
            buyer: ctx.accounts.buyer.key(),
            mint: listing.mint,
            price: sale_price,
            platform_fee,
        });

        Ok(())
    }

    /// Emergency function to recover expired listings
    pub fn recover_expired_listing(ctx: Context<RecoverExpiredListing>) -> Result<()> {
        let listing = &ctx.accounts.listing;
        require!(listing.is_active, ListingError::ListingNotActive);
        
        // Check if listing has expired
        if let Some(expiry) = listing.expiry {
            let clock = Clock::get()?;
            require!(clock.unix_timestamp > expiry, ListingError::ListingNotExpired);
        } else {
            return Err(ListingError::ListingHasNoExpiry.into());
        }

        // Transfer NFT back to seller
        let listing_key = listing.key();
        let seeds = &[
            b"listing",
            listing_key.as_ref(),
            &[listing.bump],
        ];
        let signer = &[&seeds[..]];

        let transfer_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.listing_token_account.to_account_info(),
                to: ctx.accounts.seller_token_account.to_account_info(),
                authority: ctx.accounts.listing.to_account_info(),
            },
            signer,
        );
        token::transfer(transfer_ctx, 1)?;

        // Mark listing as inactive
        let listing = &mut ctx.accounts.listing;
        listing.is_active = false;

        emit!(ExpiredListingRecovered {
            listing: listing.key(),
            seller: listing.seller,
            mint: listing.mint,
        });

        Ok(())
    }
}

#[derive(Accounts)]
pub struct ListNft<'info> {
    #[account(
        init,
        payer = seller,
        space = 8 + ListingState::INIT_SPACE,
        seeds = [b"listing", mint.key().as_ref(), seller.key().as_ref()],
        bump
    )]
    pub listing: Account<'info, ListingState>,
    
    #[account(mut)]
    pub seller: Signer<'info>,
    
    pub mint: Account<'info, Mint>,
    
    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = seller,
        constraint = seller_token_account.amount == 1
    )]
    pub seller_token_account: Account<'info, TokenAccount>,
    
    #[account(
        init,
        payer = seller,
        associated_token::mint = mint,
        associated_token::authority = listing
    )]
    pub listing_token_account: Account<'info, TokenAccount>,
    
    /// CHECK: Metadata account for the NFT - using UncheckedAccount since mpl_token_metadata::accounts::Metadata doesn't implement required traits
    #[account(
        constraint = metadata.key() == find_metadata_account(&mint.key()).0
    )]
    pub metadata: UncheckedAccount<'info>,
    
    pub marketplace: Account<'info, marketplace::MarketplaceState>,
    
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct UpdateListing<'info> {
    #[account(
        mut,
        seeds = [b"listing", listing.mint.as_ref(), listing.seller.as_ref()],
        bump = listing.bump,
        has_one = seller
    )]
    pub listing: Account<'info, ListingState>,
    
    pub seller: Signer<'info>,
}

#[derive(Accounts)]
pub struct CancelListing<'info> {
    #[account(
        mut,
        seeds = [b"listing", listing.mint.as_ref(), listing.seller.as_ref()],
        bump = listing.bump,
        has_one = seller
    )]
    pub listing: Account<'info, ListingState>,
    
    #[account(mut)]
    pub seller: Signer<'info>,
    
    #[account(
        mut,
        associated_token::mint = listing.mint,
        associated_token::authority = listing
    )]
    pub listing_token_account: Account<'info, TokenAccount>,
    
    #[account(
        mut,
        associated_token::mint = listing.mint,
        associated_token::authority = seller
    )]
    pub seller_token_account: Account<'info, TokenAccount>,
    
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct BuyNft<'info> {
    #[account(
        mut,
        seeds = [b"listing", listing.mint.as_ref(), listing.seller.as_ref()],
        bump = listing.bump
    )]
    pub listing: Account<'info, ListingState>,
    
    #[account(mut)]
    pub buyer: Signer<'info>,
    
    /// CHECK: Seller account for payment
    #[account(
        mut,
        constraint = seller.key() == listing.seller
    )]
    pub seller: AccountInfo<'info>,
    
    #[account(
        mut,
        associated_token::mint = listing.mint,
        associated_token::authority = listing
    )]
    pub listing_token_account: Account<'info, TokenAccount>,
    
    #[account(
        init_if_needed,
        payer = buyer,
        associated_token::mint = mint,
        associated_token::authority = buyer
    )]
    pub buyer_token_account: Account<'info, TokenAccount>,

    #[account(
        constraint = mint.key() == listing.mint
    )]
    pub mint: Account<'info, Mint>,
    
    #[account(mut)]
    pub marketplace: Account<'info, marketplace::MarketplaceState>,
    
    /// CHECK: Treasury account from marketplace
    #[account(
        mut,
        constraint = treasury.key() == marketplace.treasury
    )]
    pub treasury: AccountInfo<'info>,
    
    /// CHECK: Marketplace program for CPI
    pub marketplace_program: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct RecoverExpiredListing<'info> {
    #[account(
        mut,
        seeds = [b"listing", listing.mint.as_ref(), listing.seller.as_ref()],
        bump = listing.bump
    )]
    pub listing: Account<'info, ListingState>,
    
    /// CHECK: Can be called by anyone for expired listings
    pub caller: Signer<'info>,
    
    #[account(
        mut,
        associated_token::mint = listing.mint,
        associated_token::authority = listing
    )]
    pub listing_token_account: Account<'info, TokenAccount>,
    
    #[account(
        mut,
        associated_token::mint = listing.mint,
        associated_token::authority = listing.seller
    )]
    pub seller_token_account: Account<'info, TokenAccount>,
    
    pub token_program: Program<'info, Token>,
}

#[account]
#[derive(InitSpace)]
pub struct ListingState {
    pub seller: Pubkey,              // 32
    pub mint: Pubkey,                // 32
    pub price: u64,                  // 8
    pub created_at: i64,             // 8
    pub expiry: Option<i64>,         // 1 + 8
    pub is_active: bool,             // 1
    pub bump: u8,                    // 1
}

impl ListingState {
    pub const INIT_SPACE: usize = 32 + 32 + 8 + 8 + 1 + 8 + 1 + 1; // 91 bytes
}

// Helper function to find metadata account
pub fn find_metadata_account(mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            b"metadata",
            &mpl_token_metadata::ID.to_bytes(),
            &mint.to_bytes(),
        ],
        &mpl_token_metadata::ID,
    )
}

#[event]
pub struct NftListed {
    pub listing: Pubkey,
    pub seller: Pubkey,
    pub mint: Pubkey,
    pub price: u64,
    pub expiry: Option<i64>,
}

#[event]
pub struct ListingUpdated {
    pub listing: Pubkey,
    pub seller: Pubkey,
    pub old_price: u64,
    pub new_price: u64,
    pub new_expiry: Option<i64>,
}

#[event]
pub struct ListingCanceled {
    pub listing: Pubkey,
    pub seller: Pubkey,
    pub mint: Pubkey,
}

#[event]
pub struct NftSold {
    pub listing: Pubkey,
    pub seller: Pubkey,
    pub buyer: Pubkey,
    pub mint: Pubkey,
    pub price: u64,
    pub platform_fee: u64,
}

#[event]
pub struct ExpiredListingRecovered {
    pub listing: Pubkey,
    pub seller: Pubkey,
    pub mint: Pubkey,
}

#[error_code]
pub enum ListingError {
    #[msg("Invalid price provided")]
    InvalidPrice,
    #[msg("Listing expiry must be in the future")]
    InvalidExpiry,
    #[msg("Listing is not active")]
    ListingNotActive,
    #[msg("Listing has expired")]
    ListingExpired,
    #[msg("Listing has not expired yet")]
    ListingNotExpired,
    #[msg("Listing has no expiry date")]
    ListingHasNoExpiry,
    #[msg("Marketplace is currently paused")]
    MarketplacePaused,
    #[msg("Math overflow occurred")]
    MathOverflow,
    #[msg("Insufficient funds")]
    InsufficientFunds,
}

// Re-export for external access
pub use marketplace::MarketplaceState;